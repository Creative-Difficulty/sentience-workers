-- Shared deterministic dev/e2e seed for unidb (load AFTER schema/*.sql).
-- Synthetic data only. Deterministic: uses setseed + fixed ids.
SELECT setseed(0.42);

INSERT INTO discord_channels (channel_id, name, channel_type, parent_channel_id) VALUES
  (1000, 'Text Channels', 'category', NULL),
  (1001, 'general', 'text', 1000),
  (1002, 'rust-help', 'text', 1000),
  (1003, 'off-topic-with-a-really-long-channel-name-to-stress-layout', 'text', 1000),
  (1004, 'showcase', 'forum', 1000),
  (1005, 'voice-lounge', 'voice', 1000),
  (1006, 'borrow-checker-thread', 'public_thread', 1002);

INSERT INTO vestibule_users (id, nickname)
SELECT ('00000000-0000-4000-8000-' || lpad(i::text, 12, '0'))::uuid, 'User ' || i
FROM generate_series(1, 12) AS i;

INSERT INTO discord_accounts (discord_user_id, vestibule_user_id, username, display_name)
SELECT 100000000000000000 + i,
       ('00000000-0000-4000-8000-' || lpad(i::text, 12, '0'))::uuid,
       CASE WHEN i = 7 THEN 'averyveryverylongusernamethatkeepsgoing_' || i ELSE 'user_' || i END,
       CASE WHEN i = 3 THEN 'Ünïcødé 🦀 Display Name' ELSE 'Display ' || i END
FROM generate_series(1, 12) AS i;

-- ~1500 messages spread over the last 45 days, more in the last 24h.
INSERT INTO messages (message_id, channel_id, sent_by, content, sent_at)
SELECT 200000000000000000 + n,
       (ARRAY[1001,1002,1003,1004,1006])[1 + (n % 5)],
       100000000000000000 + 1 + (n * 7 % 12),
       (ARRAY[
         'has anyone tried the new borrow checker diagnostics? they are much better now',
         'I live in Vienna and have been writing Rust professionally since 2021',
         'lunch plans? thinking ramen near the office',
         'deploy failed last night, migration did not run',
         'rolling back the migration now',
         'ichiran on 5th is solid for ramen',
         'just finished my first marathon!',
         'here is a screenshot of my new keyboard build',
         'https://example.com/a-really-long-url-without-any-spaces-that-should-wrap-correctly-in-the-explorer-view-and-not-overflow-the-container-horizontally',
         E'multi\nline\nmessage with <b>html</b> & <script>alert(1)</script> to test escaping'
       ])[1 + (n % 10)],
       CASE WHEN n % 4 = 0 THEN now() - (random() * interval '24 hours')
            ELSE now() - (random() * interval '45 days') END
FROM generate_series(1, 1500) AS n;

-- some replies
UPDATE messages SET in_reply_to = message_id - 5
WHERE message_id % 9 = 0 AND message_id - 5 > 200000000000000000;
-- a few edits / deletions
UPDATE messages SET last_edited = sent_at + interval '5 minutes' WHERE message_id % 31 = 0;
INSERT INTO message_edits (message_id, old_content, edited_at)
SELECT message_id, 'original: ' || content, sent_at + interval '5 minutes' FROM messages WHERE message_id % 31 = 0;
UPDATE messages SET deleted_at = sent_at + interval '1 hour' WHERE message_id % 53 = 0;

-- emojis & reactions
INSERT INTO discord_emojis (id, discord_emoji_id, emoji_display_name) VALUES
  ('10000000-0000-4000-8000-000000000001', '👍', NULL),
  ('10000000-0000-4000-8000-000000000002', '🦀', NULL),
  ('10000000-0000-4000-8000-000000000003', '😂', NULL);
INSERT INTO message_reactions (message_id, user_id, emoji_id)
SELECT m.message_id, 100000000000000000 + u, ('10000000-0000-4000-8000-00000000000' || (1 + (m.message_id + u) % 3))::uuid
FROM messages m, generate_series(1, 4) AS u
WHERE m.message_id % 6 = 0
ON CONFLICT DO NOTHING;

-- attachments
INSERT INTO media_assets (id, content_type, object_key, size_bytes, content_hash)
SELECT ('20000000-0000-4000-8000-' || lpad(n::text, 12, '0'))::uuid, 'image/png',
       'discord/attachments/' || (200000000000000000 + n) || '/' || n, 12345 + n, md5(n::text)
FROM generate_series(8, 1500, 10) AS n;
INSERT INTO message_attachments (message_id, asset_id)
SELECT 200000000000000000 + n, ('20000000-0000-4000-8000-' || lpad(n::text, 12, '0'))::uuid
FROM generate_series(8, 1500, 10) AS n;

-- topics
INSERT INTO topic (id, name) VALUES
  ('30000000-0000-4000-8000-000000000001', 'rust borrow checker'),
  ('30000000-0000-4000-8000-000000000002', 'ramen recommendations'),
  ('30000000-0000-4000-8000-000000000003', 'deploy failure'),
  ('30000000-0000-4000-8000-000000000004', 'running and marathons');
INSERT INTO topic_message_relation (topic_id, message_id)
SELECT ('30000000-0000-4000-8000-00000000000' || (1 + (message_id % 4)))::uuid, message_id
FROM messages WHERE message_id % 3 = 0;
INSERT INTO message_classification_attempts (message_id) SELECT message_id FROM messages WHERE message_id % 2 = 0;

-- facts (+ required evidence, inserted in one tx because of the deferred trigger)
BEGIN;
INSERT INTO user_facts_and_activities (id, user_id, record_type, source, type, value, confidence, level, started_at, is_current)
SELECT ('40000000-0000-4000-8000-' || lpad(i::text, 12, '0'))::uuid,
       ('00000000-0000-4000-8000-' || lpad((1 + i % 12)::text, 12, '0'))::uuid,
       (ARRAY['fact','skill','activity']::activity_record_type[])[1 + i % 3],
       'llm_extraction',
       (ARRAY['location','programming_language','race_completed'])[1 + i % 3],
       (ARRAY['Vienna','Rust','Vienna marathon (third marathon)'])[1 + i % 3],
       0.7 + (i % 3) * 0.1,
       CASE WHEN i % 3 = 1 THEN 1 + i % 10 ELSE NULL END,
       CASE WHEN i % 3 = 2 THEN now() - interval '10 days' ELSE NULL END,
       true
FROM generate_series(1, 40) AS i;
INSERT INTO fact_and_activity_evidence (fact_or_activity_id, message_id, weight, reasoning)
SELECT ('40000000-0000-4000-8000-' || lpad(i::text, 12, '0'))::uuid, 200000000000000000 + i * 10 + 2, 0.9,
       'Author states it explicitly.'
FROM generate_series(1, 40) AS i;
COMMIT;
INSERT INTO fact_extraction_attempts (message_id) SELECT message_id FROM messages WHERE message_id % 2 = 0;
