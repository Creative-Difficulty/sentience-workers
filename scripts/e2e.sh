#!/usr/bin/env bash
# End-to-end test of the full local stack:
#   postgres(+unidb schema) -> seed -> rustfs + bucket -> mock LLM
#   -> fact-extractor + topic-sorter -> dashboard
#
# 1. builds and starts the default compose stack under a unique project name
# 2. loads docker/seed/dev-seed.sql and deletes its *_attempts rows, so the
#    workers have to process every seeded message themselves
# 3. waits until fact_extraction_attempts / message_classification_attempts
#    cover all eligible messages and mock-LLM facts and topics exist
# 4. checks the dashboard pages and /api/messages over HTTP
# 5. tears everything down (volumes included); non-zero exit on any failure
#
# Requires: bash, docker (compose v2), curl, python3.
# Env:
#   E2E_PROJECT       compose project name     (default: sentience-e2e-<pid>)
#   E2E_TIMEOUT       seconds to wait for the workers (default: 600)
#   E2E_KEEP=1        leave the stack running afterwards
#   E2E_NO_BUILD=1    do not build; use existing images named <project>-<service>
#   E2E_DASH_PORT / E2E_POSTGRES_PORT / E2E_RUSTFS_PORT
#                     fixed loopback host ports (default: random free ports)
#   DASH_CONTEXT      dashboard build context (see docker-compose.yml)
#   COMPOSE_FILE      extra compose files, as usual for docker compose
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

PROJECT="${E2E_PROJECT:-sentience-e2e-$$}"
TIMEOUT="${E2E_TIMEOUT:-600}"
SEED_FILE="docker/seed/dev-seed.sql"

export COMPOSE_PROJECT_NAME="$PROJECT"
# "127.0.0.1:" + ":<container port>" => docker picks a free loopback port.
export POSTGRES_PUBLISH="127.0.0.1:${E2E_POSTGRES_PORT:-}"
export RUSTFS_CONSOLE_PUBLISH="127.0.0.1:${E2E_RUSTFS_PORT:-}"
export DASH_PUBLISH="127.0.0.1:${E2E_DASH_PORT:-}"
# Never talk to a real LLM from the e2e test, whatever .env says.
export LLM_BASE_URL="http://mock-llm:8080/v1" LLM_API_KEY="mock" LLM_MODEL="mock"

FAILURES=0
BODY=$(mktemp)
log() { printf '[e2e %(%H:%M:%S)T] %s\n' -1 "$*"; }
pass() { log "PASS: $*"; }
fail() { log "FAIL: $*"; FAILURES=$((FAILURES + 1)); }
dc() { docker compose "$@"; }
psql_q() {
    dc exec -T postgres sh -c 'psql -X -v ON_ERROR_STOP=1 -At -U "$POSTGRES_USER" -d "$POSTGRES_DB"' <<<"$1"
}

cleanup() {
    local rc=$?
    if [[ $rc -ne 0 || $FAILURES -ne 0 ]]; then
        log "--- service status ---"
        dc ps -a || true
        log "--- last log lines ---"
        dc logs --no-color --tail=40 || true
    fi
    rm -f "$BODY"
    if [[ "${E2E_KEEP:-0}" == "1" ]]; then
        log "E2E_KEEP=1: leaving project '$PROJECT' running (docker compose -p $PROJECT down -v)"
    else
        log "tearing down project '$PROJECT'"
        dc down -v --remove-orphans >/dev/null 2>&1 || true
    fi
    if [[ $rc -eq 0 && $FAILURES -eq 0 ]]; then log "E2E RESULT: PASS"; else log "E2E RESULT: FAIL (rc=$rc, failures=$FAILURES)"; fi
    exit $(( rc != 0 ? rc : (FAILURES != 0) ))
}
trap cleanup EXIT

# wait_for <description> <timeout-seconds> <command...>
wait_for() {
    local what=$1 limit=$2 start=$SECONDS
    shift 2
    until "$@"; do
        if (( SECONDS - start >= limit )); then
            fail "timed out after ${limit}s waiting for: $what"
            return 1
        fi
        sleep 3
    done
    pass "$what (after $((SECONDS - start))s)"
}

log "project=$PROJECT timeout=${TIMEOUT}s dash_context=${DASH_CONTEXT:-<compose default>}"

UP_FLAGS=()
if [[ "${E2E_NO_BUILD:-0}" == "1" ]]; then
    log "E2E_NO_BUILD=1: using pre-built images"
    UP_FLAGS+=(--no-build)
else
    log "building images"
    dc build --quiet
fi

log "starting postgres"
dc up -d "${UP_FLAGS[@]}" --wait --wait-timeout 180 postgres

log "loading $SEED_FILE (without attempt rows)"
{
    cat "$SEED_FILE"
    echo "DELETE FROM fact_extraction_attempts; DELETE FROM message_classification_attempts;"
} | dc exec -T postgres sh -c 'psql -X -q -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"' >/dev/null

# Messages each worker is expected to process (mirrors their batch queries).
FE_ELIGIBLE="FROM messages m WHERE m.deleted_at IS NULL AND m.content != '[deleted message]' AND char_length(m.content) >= 10"
TS_ELIGIBLE="FROM messages m WHERE m.content != '[deleted message]'"
seeded=$(psql_q "SELECT count(*) FROM messages")
fe_total=$(psql_q "SELECT count(*) $FE_ELIGIBLE")
ts_total=$(psql_q "SELECT count(*) $TS_ELIGIBLE")
attempts=$(psql_q "SELECT (SELECT count(*) FROM fact_extraction_attempts) + (SELECT count(*) FROM message_classification_attempts)")
log "seeded messages=$seeded fact-extractor eligible=$fe_total topic-sorter eligible=$ts_total attempt rows=$attempts"
[[ $seeded -gt 0 && $attempts -eq 0 ]] || { fail "seed did not load as expected"; exit 1; }

log "starting the rest of the default stack"
dc up -d "${UP_FLAGS[@]}" --wait --wait-timeout 300

fe_pending() { psql_q "SELECT count(*) $FE_ELIGIBLE AND NOT EXISTS (SELECT 1 FROM fact_extraction_attempts a WHERE a.message_id = m.message_id)"; }
ts_pending() { psql_q "SELECT count(*) $TS_ELIGIBLE AND NOT EXISTS (SELECT 1 FROM message_classification_attempts a WHERE a.message_id = m.message_id)"; }
fe_done() { local p; p=$(fe_pending); log "fact-extractor pending: $p/$fe_total"; [[ $p -eq 0 ]]; }
ts_done() { local p; p=$(ts_pending); log "topic-sorter pending: $p/$ts_total"; [[ $p -eq 0 ]]; }

wait_for "fact_extraction_attempts cover all $fe_total eligible messages" "$TIMEOUT" fe_done
wait_for "message_classification_attempts cover all $ts_total eligible messages" "$TIMEOUT" ts_done

mock_facts=$(psql_q "SELECT count(DISTINCT e.fact_or_activity_id) FROM fact_and_activity_evidence e WHERE e.reasoning LIKE '[mock-llm]%'")
mock_rel=$(psql_q "SELECT count(*) FROM topic_message_relation r JOIN topic t ON t.id = r.topic_id WHERE t.name LIKE 'mock %'")
mock_topics=$(psql_q "SELECT string_agg(name, ', ' ORDER BY name) FROM topic WHERE name LIKE 'mock %'")
if [[ $mock_facts -gt 0 ]]; then pass "fact-extractor stored $mock_facts facts from the mock LLM"; else fail "no mock-LLM facts stored"; fi
if [[ $mock_rel -gt 0 ]]; then pass "topic-sorter assigned $mock_rel messages to mock topics ($mock_topics)"; else fail "no mock topics assigned"; fi

# ─── dashboard ──────────────────────────────────────────────────────────
dash_addr=$(dc port dash 3000)
BASE="http://${dash_addr}"
log "dashboard at $BASE"
wait_for "dashboard /healthz?db=1 answers 200" 120 \
    sh -c "[ \"\$(curl -s -o /dev/null -w '%{http_code}' '$BASE/healthz?db=1')\" = 200 ]"

# check_page <path> <expected status> <expected substring>...
check_page() {
    local path=$1 want=$2 code
    shift 2
    code=$(curl -sS -o "$BODY" -w '%{http_code}' "$BASE$path") || code=000
    if [[ $code != "$want" ]]; then fail "GET $path -> $code (want $want)"; return; fi
    local needle
    for needle in "$@"; do
        if ! grep -qF -- "$needle" "$BODY"; then fail "GET $path: body lacks '$needle'"; return; fi
    done
    # Loaders return {error: ...} on DB failures instead of a 500; catch that too.
    if grep -qE 'DATABASE_URL is not set|ECONNREFUSED|error:"' "$BODY"; then fail "GET $path: page reports a server error"; return; fi
    pass "GET $path -> $code${*:+ (contains: $*)}"
}

user_id=$(psql_q "SELECT f.user_id FROM user_facts_and_activities f JOIN fact_and_activity_evidence e ON e.fact_or_activity_id = f.id WHERE e.reasoning LIKE '[mock-llm]%' ORDER BY f.user_id LIMIT 1")
fact_id=$(psql_q "SELECT f.id FROM user_facts_and_activities f JOIN fact_and_activity_evidence e ON e.fact_or_activity_id = f.id WHERE e.reasoning LIKE '[mock-llm]%' AND f.user_id = '$user_id' ORDER BY f.created_at DESC LIMIT 1")
username=$(psql_q "SELECT username FROM discord_accounts WHERE vestibule_user_id = '$user_id' LIMIT 1")

check_page "/" 200 "general" "rust-help"
check_page "/explorer" 200 "rust-help" "showcase"
check_page "/userdata" 200 "accounts tracked" "$username"
check_page "/userdata/$user_id" 200 "Identity" "$username" "$fact_id"
check_page "/userdata/00000000-0000-4000-8000-0000000000ff" 404

# A message the topic-sorter put into a mock topic (and not into a seed topic).
MOCK_MSG="FROM topic_message_relation r JOIN topic t ON t.id = r.topic_id JOIN messages m ON m.message_id = r.message_id WHERE t.name LIKE 'mock %' AND m.deleted_at IS NULL ORDER BY m.message_id DESC LIMIT 1"
msg_id=$(psql_q "SELECT m.message_id $MOCK_MSG")
channel_id=$(psql_q "SELECT m.channel_id $MOCK_MSG")
topic_name=$(psql_q "SELECT t.name $MOCK_MSG")
api="/api/messages?channelId=${channel_id}&before=$((msg_id + 1))"
code=$(curl -sS -o "$BODY" -w '%{http_code}' "$BASE$api") || code=000
if [[ $code != 200 ]]; then
    fail "GET $api -> $code"
elif python3 -I - "$BODY" "$msg_id" "$topic_name" <<'PY'
import json, sys
path, msg_id, topic = sys.argv[1:]
data = json.load(open(path))
msgs = data["messages"]
hit = [m for m in msgs if m["messageId"] == msg_id]
assert hit, f"message {msg_id} not in response ({len(msgs)} messages)"
got = (hit[0].get("topic") or {}).get("topicName")
assert got == topic, f"message {msg_id}: topic {got!r}, want {topic!r}"
assert any(m.get("topic") for m in msgs), "no topics at all"
print(f"{len(msgs)} messages, message {msg_id} has topic {got!r}")
PY
then
    pass "GET $api returns JSON with mock topic '$topic_name' on message $msg_id"
else
    fail "GET $api: JSON check failed"
fi
check_page "/api/messages" 400

if [[ $FAILURES -ne 0 ]]; then exit 1; fi
