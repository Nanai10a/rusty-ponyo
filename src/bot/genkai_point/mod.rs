pub(crate) mod model;

use {
    crate::{
        bot::{
            genkai_point::model::{Session, UserStat},
            BotService, Context, Message, SendMessage,
        },
        Synced, ThreadSafe,
    },
    anyhow::{Context as _, Result},
    async_trait::async_trait,
    chrono::{DateTime, Utc},
    std::marker::PhantomData,
};

#[async_trait]
pub(crate) trait GenkaiPointDatabase: ThreadSafe {
    /// Creates a new unclosed session if not exists.
    /// If an unclosed session exists, leaves it untouched.
    /// Returns whether it's created.
    async fn create_new_session(&mut self, user_id: u64, joined_at: DateTime<Utc>) -> Result<bool>;
    async fn unclosed_session_exists(&self, user_id: u64) -> Result<bool>;
    async fn close_session(&mut self, user_id: u64, left_at: DateTime<Utc>) -> Result<()>;
    async fn get_users_all_sessions(&self, user_id: u64) -> Result<Vec<Session>>;
    async fn get_all_users_who_has_unclosed_session(&self) -> Result<Vec<u64>>;
    async fn get_all_users_stats(&self) -> Result<Vec<UserStat>>;
}

pub(crate) struct GenkaiPointBot<D>(PhantomData<fn() -> D>);

impl<D: GenkaiPointDatabase> GenkaiPointBot<D> {
    pub(crate) fn new() -> Self {
        Self(PhantomData)
    }

    async fn ranking<F, O>(
        &self,
        db: &Synced<D>,
        ctx: &dyn Context,
        sort_msg: &str,
        sort_key_selector: F,
    ) -> Result<String>
    where
        F: Fn(&UserStat) -> O,
        O: Ord,
    {
        let mut ranking = db
            .read()
            .await
            .get_all_users_stats()
            .await
            .context("failed to fetch ranking")?;

        ranking.sort_by_key(|x| x.user_id);
        ranking.sort_by_key(sort_key_selector);

        let mut result = vec!["```".to_string(), sort_msg.to_string()];

        let iter = ranking.iter().rev().take(20).enumerate();

        for (index, stat) in iter {
            let username = ctx
                .get_user_name(stat.user_id)
                .await
                .context("failed to get username")?;

            result.push(format!(
                "#{:02} {:4}pt. {:4.2}h {}",
                index + 1,
                stat.genkai_point,
                (stat.total_vc_duration.num_seconds() as f64) / 3600.,
                username
            ))
        }

        result.push("```".to_string());

        Ok(result.join("\n"))
    }
}

#[async_trait]
impl<D: GenkaiPointDatabase> BotService for GenkaiPointBot<D> {
    const NAME: &'static str = "GenkaiPointBot";
    type Database = D;

    async fn on_message(
        &self,
        db: &Synced<Self::Database>,
        msg: &dyn Message,
        ctx: &dyn Context,
    ) -> Result<()> {
        let tokens = msg.content().split_ascii_whitespace().collect::<Vec<_>>();

        const PREFIX: &str = "g!point";

        let msg = match tokens.as_slice() {
            [] => None,
            [maybe_prefix, ..] if *maybe_prefix != PREFIX => None,

            [_, "ranking", "duration"] => Some(
                self.ranking(db, ctx, "sorted by vc duration", |x| x.total_vc_duration)
                    .await?,
            ),

            [_, "ranking", "point"] | [_, "ranking"] => Some(
                self.ranking(db, ctx, "sorted by genkai point", |x| x.genkai_point)
                    .await?,
            ),

            [_, "show", ..] | [_, "限界ポイント", ..] => {
                let sessions = db
                    .read()
                    .await
                    .get_users_all_sessions(msg.author().id())
                    .await
                    .context("failed to get sessions")?;

                let points = sessions.iter().map(|x| x.calc_point()).sum::<u64>();

                let vc_hour = sessions
                    .iter()
                    .map(|x| x.duration())
                    .map(|x| (x.num_seconds() as f64) / 3600.)
                    .sum::<f64>();

                Some(format!(
                    "```\n{name}\n  - points: {points}\n  - total vc duration: {vc_hour:.2} h \n```",
                    name = msg.author().name(),
                    points = points,
                    vc_hour = vc_hour
                ))
            }

            #[rustfmt::skip]
            [_, ..] => Some(
r#"```asciidoc
= rusty_ponyo::genkai_point =
g!point [subcommand] [args...]

= subcommands =
    help                        :: この文を出します
    show                        :: あなたの限界ポイントなどを出します
    ranking [duration or point] :: ランキングを出します
```"#.into()
            ),
        };

        if let Some(msg) = msg {
            ctx.send_message(SendMessage {
                content: &msg,
                attachments: &[],
            })
            .await
            .context("failed to send message")?;
        }

        Ok(())
    }

    async fn on_vc_join(
        &self,
        db: &Synced<Self::Database>,
        _ctx: &dyn Context,
        user_id: u64,
    ) -> Result<()> {
        db.write()
            .await
            .create_new_session(user_id, Utc::now())
            .await
            .context("failed to create new session")?;

        Ok(())
    }

    async fn on_vc_leave(
        &self,
        db: &Synced<Self::Database>,
        ctx: &dyn Context,
        user_id: u64,
    ) -> Result<()> {
        db.write()
            .await
            .close_session(user_id, Utc::now())
            .await
            .context("failed to close session")?;

        let mut sessions = db
            .read()
            .await
            .get_users_all_sessions(user_id)
            .await
            .context("failed to get all closed sessions")?;

        sessions.sort_by_key(|x| x.left_at);

        let this_time_point = sessions.last().unwrap().calc_point();

        if this_time_point > 0 {
            let sum = sessions.iter().map(|x| x.calc_point()).sum::<u64>();

            let msg = format!(
                "now <@!{}> has {} genkai point (+{})",
                user_id, sum, this_time_point
            );

            ctx.send_message(SendMessage {
                content: &msg,
                attachments: &[],
            })
            .await
            .context("failed to send message")?;
        }

        Ok(())
    }

    async fn on_vc_data_available(
        &self,
        db: &Synced<Self::Database>,
        _ctx: &dyn Context,
        joined_user_ids: &[u64],
    ) -> Result<()> {
        for uid in joined_user_ids {
            let created = db
                .write()
                .await
                .create_new_session(*uid, Utc::now())
                .await
                .context("failed to create new session")?;

            if !created {
                tracing::info!("User({}) already has unclosed session in db", uid);
            } else {
                tracing::info!("User({}) has joined to vc in bot downtime", uid);
            }
        }

        let db_state = db
            .read()
            .await
            .get_all_users_who_has_unclosed_session()
            .await
            .context("failed to get users who has unclosed session")?;

        for uid in db_state {
            if joined_user_ids.contains(&uid) {
                continue;
            }

            db.write()
                .await
                .close_session(uid, Utc::now())
                .await
                .context("failed to close session")?;

            tracing::info!("User({}) has left from vc in bot downtime", uid);
        }

        Ok(())
    }
}
