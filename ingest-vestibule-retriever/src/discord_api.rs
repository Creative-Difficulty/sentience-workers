//! Thin abstraction over the handful of Discord REST calls the ingester makes.
//!
//! The production implementation ([`SerenityDiscord`]) forwards to serenity's [`Http`] client.
//! Tests provide their own implementation backed by JSON fixtures so that every DB-writing code
//! path can be exercised without contacting Discord.

use std::sync::Arc;

use serenity::all::{
    Channel, ChannelId, GuildChannel, GuildId, Http, HttpError, Message, MessageId,
    MessagePagination, ReactionType, StatusCode, User, UserId,
};
use serenity::async_trait;

/// One page of threads as returned by the archived-threads endpoints.
#[derive(Debug, Clone, Default)]
pub struct ThreadPage {
    pub threads: Vec<GuildChannel>,
    pub has_more: bool,
}

#[async_trait]
pub trait DiscordApi: Send + Sync {
    /// Fetches a channel. Errors on any failure (including 404/403).
    async fn get_channel(&self, channel_id: ChannelId) -> color_eyre::Result<Channel>;

    /// Fetches a single message. Returns `Ok(None)` when Discord answers 404.
    async fn get_message(
        &self,
        channel_id: ChannelId,
        message_id: MessageId,
    ) -> color_eyre::Result<Option<Message>>;

    /// Fetches up to 100 users that reacted with `reaction` on a message, after `after`.
    async fn reaction_users(
        &self,
        channel_id: ChannelId,
        message_id: MessageId,
        reaction: &ReactionType,
        after: Option<UserId>,
    ) -> color_eyre::Result<Vec<User>>;

    /// Fetches a user.
    async fn get_user(&self, user_id: UserId) -> color_eyre::Result<User>;

    /// All (non-thread) channels of a guild.
    async fn guild_channels(&self, guild_id: GuildId) -> color_eyre::Result<Vec<GuildChannel>>;

    /// Up to 100 messages of a channel. `before` paginates towards older messages.
    /// Discord returns messages newest-first.
    async fn messages_before(
        &self,
        channel_id: ChannelId,
        before: Option<MessageId>,
    ) -> color_eyre::Result<Vec<Message>>;

    /// Up to 100 messages of a channel whose id is greater than `after` (i.e. the 100 oldest
    /// messages after `after`). The order of the returned vector is not relied upon.
    async fn messages_after(
        &self,
        channel_id: ChannelId,
        after: MessageId,
    ) -> color_eyre::Result<Vec<Message>>;

    /// All active (non-archived) threads of a guild.
    async fn active_threads(&self, guild_id: GuildId) -> color_eyre::Result<Vec<GuildChannel>>;

    /// One page of archived public threads of a channel, archived before `before` (ISO8601).
    async fn archived_public_threads(
        &self,
        channel_id: ChannelId,
        before: Option<String>,
    ) -> color_eyre::Result<ThreadPage>;
}

/// Production [`DiscordApi`] backed by serenity's HTTP client.
#[derive(Clone)]
pub struct SerenityDiscord(pub Arc<Http>);

fn is_not_found(err: &serenity::Error) -> bool {
    matches!(
        err,
        serenity::Error::Http(HttpError::UnsuccessfulRequest(res))
            if res.status_code == StatusCode::NOT_FOUND
    )
}

#[async_trait]
impl DiscordApi for SerenityDiscord {
    async fn get_channel(&self, channel_id: ChannelId) -> color_eyre::Result<Channel> {
        Ok(self.0.get_channel(channel_id).await?)
    }

    async fn get_message(
        &self,
        channel_id: ChannelId,
        message_id: MessageId,
    ) -> color_eyre::Result<Option<Message>> {
        match self.0.get_message(channel_id, message_id).await {
            Ok(m) => Ok(Some(m)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn reaction_users(
        &self,
        channel_id: ChannelId,
        message_id: MessageId,
        reaction: &ReactionType,
        after: Option<UserId>,
    ) -> color_eyre::Result<Vec<User>> {
        Ok(self
            .0
            .get_reaction_users(
                channel_id,
                message_id,
                reaction,
                100,
                after.map(|u| u.get()),
            )
            .await?)
    }

    async fn get_user(&self, user_id: UserId) -> color_eyre::Result<User> {
        Ok(self.0.get_user(user_id).await?)
    }

    async fn guild_channels(&self, guild_id: GuildId) -> color_eyre::Result<Vec<GuildChannel>> {
        Ok(self.0.get_channels(guild_id).await?)
    }

    async fn messages_before(
        &self,
        channel_id: ChannelId,
        before: Option<MessageId>,
    ) -> color_eyre::Result<Vec<Message>> {
        Ok(self
            .0
            .get_messages(channel_id, before.map(MessagePagination::Before), Some(100))
            .await?)
    }

    async fn messages_after(
        &self,
        channel_id: ChannelId,
        after: MessageId,
    ) -> color_eyre::Result<Vec<Message>> {
        Ok(self
            .0
            .get_messages(channel_id, Some(MessagePagination::After(after)), Some(100))
            .await?)
    }

    async fn active_threads(&self, guild_id: GuildId) -> color_eyre::Result<Vec<GuildChannel>> {
        Ok(self.0.get_guild_active_threads(guild_id).await?.threads)
    }

    async fn archived_public_threads(
        &self,
        channel_id: ChannelId,
        before: Option<String>,
    ) -> color_eyre::Result<ThreadPage> {
        use serenity::http::{LightMethod, Request, Route};
        // serenity's `get_channel_archived_public_threads` formats `before` as an integer, but
        // Discord expects an ISO8601 timestamp, so the request is built by hand.
        let mut params = vec![("limit", "100".to_string())];
        if let Some(before) = before {
            params.push(("before", before));
        }
        let req = Request::new(
            Route::ChannelArchivedPublicThreads { channel_id },
            LightMethod::Get,
        )
        .params(Some(params));
        let data: serenity::all::ThreadsData = self.0.fire(req).await?;
        Ok(ThreadPage {
            threads: data.threads,
            has_more: data.has_more,
        })
    }
}
