use ormlite::Model as _;
use serenity::all::{GuildId, ReactionType as SerenityReactionType, ReactionType};
use sqlx::PgPool;
use unidb::models::DiscordEmoji;
use uuid::Uuid;

pub async fn handle_emoji_resolution(
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
    reaction_type: &SerenityReactionType,
    guild_id: Option<GuildId>,
) -> color_eyre::Result<Option<Uuid>> {
    match reaction_type {
        ReactionType::Custom {
            animated, id, name, ..
        } => {
            let discord_emoji_id = id.get().to_string();

            let existing_emoji = sqlx::query_as!(
                DiscordEmoji,
                "SELECT * FROM discord_emojis WHERE discord_emoji_id = $1",
                discord_emoji_id
            )
            .fetch_optional(pool)
            .await?;

            if let Some(e) = existing_emoji {
                return Ok(Some(e.id));
            }

            // All emojis are served in webp, this makes life way easier
            let mut emoji_url = format!("https://cdn.discordapp.com/emojis/{}.webp", id.get());

            if *animated {
                emoji_url = format!(
                    "https://cdn.discordapp.com/emojis/{}.webp?animated=true",
                    id.get()
                );
            }

            // Download emoji and store as a media asset via S3 object key
            let asset_id = Uuid::new_v4();
            let object_key = format!("emojis/{}/{}.webp", id.get(), asset_id);

            super::attachments::process_and_store_media(
                pool,
                s3_client,
                s3_bucket,
                object_key,
                &emoji_url,
                "image/webp".to_string(),
            )
            .await?;

            let new_emoji_id = Uuid::new_v4();
            let new_emoji = DiscordEmoji {
                id: new_emoji_id,
                discord_emoji_id: discord_emoji_id.clone(),
                from_guild: guild_id.map(|g| g.get().to_string()),
                emoji_display_name: name.clone(),
                is_animated: *animated,
                emoji_url: Some(emoji_url),
                asset_id: Some(asset_id),
            };

            if let Err(e) = new_emoji.insert(pool).await {
                tracing::error!(error = %e, "Failed to insert discord emoji");
            }
            Ok(Some(new_emoji_id))
        }
        ReactionType::Unicode(s) => {
            let discord_emoji_id = s.clone();
            let existing_emoji = sqlx::query_as!(
                DiscordEmoji,
                "SELECT * FROM discord_emojis WHERE discord_emoji_id = $1",
                discord_emoji_id
            )
            .fetch_optional(pool)
            .await?;

            if let Some(e) = existing_emoji {
                return Ok(Some(e.id));
            }

            let new_emoji_id = Uuid::new_v4();
            let new_emoji = DiscordEmoji {
                id: new_emoji_id,
                discord_emoji_id: discord_emoji_id.clone(),
                from_guild: None,
                emoji_display_name: None,
                is_animated: false,
                emoji_url: None,
                asset_id: None,
            };
            if let Err(e) = new_emoji.insert(pool).await {
                tracing::error!(error = %e, "Failed to insert discord emoji");
            }
            Ok(Some(new_emoji_id))
        }
        _ => Ok(None),
    }
}
