use color_eyre::eyre::eyre;
use serde::Deserialize;

const SYSTEM: &str = "You are a topic classifier for Discord chat messages.
You receive earlier channel context (some messages already labeled with a topic) and a batch of new messages.
Group messages that discuss the same coherent topic into a single group.
A group may span context and new messages, and may bridge interjections from unrelated chatter —
if a topic is dropped and picked back up later, put both stretches in the same group.
Reuse existing topic names when new messages continue a known topic.
New topic names must be concise (3-7 words).
Skip messages that don't clearly belong to any multi-message topic (acknowledgments, system messages, one-off remarks).
Only emit a group when at least two messages share that topic across the whole window —
never emit a singleton group.
Return JSON of the form: {\"groups\": [{\"topic\": string, \"message_ids\": [number, ...]}]}.";

#[tracing::instrument(skip_all, fields(
    context_len = context.len(),
    new_len = new.len(),
    topics_len = topics.len(),
))]
pub async fn classify(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    context: &[(i64, String, Option<String>)],
    new: &[(i64, String)],
    topics: &[String],
) -> color_eyre::Result<Vec<(String, Vec<i64>)>> {
    let mut user = String::new();
    if !topics.is_empty() {
        user.push_str("Existing topics:\n");
        for t in topics {
            user.push_str(&format!("- {t}\n"));
        }
        user.push('\n');
    }
    if !context.is_empty() {
        user.push_str("Earlier context:\n");
        for (id, content, topic) in context {
            let c = content.replace('\n', " ");
            match topic {
                Some(t) => user.push_str(&format!("[{id}] (topic: {t}): {c}\n")),
                None => user.push_str(&format!("[{id}]: {c}\n")),
            }
        }
        user.push('\n');
    }
    user.push_str("New messages to classify:\n");
    for (id, content) in new {
        let c = content.replace('\n', " ");
        user.push_str(&format!("[{id}]: {c}\n"));
    }

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user",   "content": user}
        ]
    });

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    tracing::debug!(%url, %model, user_msg_len = user.len(), "sending classify request");

    let resp = http
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await?;
    tracing::debug!(%status, body = %text, "received LLM response");

    if !status.is_success() {
        return Err(eyre!("LLM API error {status}: {text}"));
    }

    #[derive(Deserialize)]
    struct Out {
        #[serde(default)]
        groups: Vec<G>,
    }
    #[derive(Deserialize)]
    struct G {
        topic: String,
        #[serde(default)]
        message_ids: Vec<i64>,
    }

    #[derive(Deserialize)]
    struct ApiResponse {
        choices: Vec<ApiChoice>,
    }
    #[derive(Deserialize)]
    struct ApiChoice {
        message: ApiMessage,
    }
    #[derive(Deserialize)]
    struct ApiMessage {
        content: String,
    }

    let api_resp: ApiResponse = serde_json::from_str(&text)
        .map_err(|e| eyre!("failed to parse API response envelope: {e}\nraw: {text}"))?;
    let content = api_resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| eyre!("no choices in API response\nraw: {text}"))?
        .message
        .content;
    tracing::debug!(%content, "extracted message content");

    let json = extract_json(&content);
    tracing::debug!(json, "extracted JSON from message content");

    let out: Out = serde_json::from_str(json)
        .map_err(|e| eyre!("failed to parse LLM groups JSON: {e}\nraw content: {content}"))?;

    let groups: Vec<(String, Vec<i64>)> = out
        .groups
        .into_iter()
        .filter(|g| {
            if g.message_ids.len() < 2 {
                tracing::debug!(topic = %g.topic, "dropping singleton group");
                false
            } else {
                true
            }
        })
        .map(|g| (g.topic, g.message_ids))
        .collect();

    tracing::debug!(group_count = groups.len(), "classify complete");
    Ok(groups)
}

fn extract_json(text: &str) -> &str {
    let t = text.trim();
    match (t.find('{'), t.rfind('}')) {
        (Some(start), Some(end)) if end >= start => &t[start..=end],
        _ => t,
    }
}
