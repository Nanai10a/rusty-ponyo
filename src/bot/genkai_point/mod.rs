pub(crate) mod model;

use {
    crate::{
        bot::{genkai_point::model::Session, BotService, Context, Message, SendMessage},
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
    async fn get_all_closed_sessions(&self, user_id: u64) -> Result<Vec<Session>>;
    async fn get_all_users_who_has_unclosed_session(&self) -> Result<Vec<u64>>;
}

pub(crate) struct GenkaiPointBot<D>(PhantomData<fn() -> D>);

impl<D: GenkaiPointDatabase> GenkaiPointBot<D> {
    pub(crate) fn new() -> Self {
        Self(PhantomData)
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

            [_, "show", ..] => {
                let sessions = db
                    .read()
                    .await
                    .get_all_closed_sessions(msg.author().id())
                    .await
                    .context("failed to get sessions")?;

                let points = sessions
                    .iter()
                    .map(|x| x.calc_point())
                    .sum::<Result<u64>>()
                    .context("failed to calculate genkai point")?;

                let vc_min = sessions
                    .iter()
                    .map(|x| x.duration())
                    .map(|x| x.map(|x| (x.num_seconds() as f64) / 60.))
                    .sum::<Result<f64>>()
                    .context("failed to get vc duration")?;

                Some(format!(
                    "```\n{name}\n  - points: {points}\n  - total vc duration: {vc_min:.2} min \n```",
                    name = msg.author().name(),
                    points = points,
                    vc_min = vc_min
                ))
            }

            #[rustfmt::skip]
            [_, ..] => Some(
r#"```asciidoc
= rusty_ponyo::genkai_point =
g!point [subcommand] [args...]

= subcommands =
    help :: この文を出します
    show :: あなたの限界ポイントなどを出します
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
            .get_all_closed_sessions(user_id)
            .await
            .context("failed to get all closed sessions")?;

        sessions.sort_by_key(|x| x.left_at);

        let this_time_point = sessions.last().unwrap().calc_point().unwrap();

        if this_time_point > 0 {
            let sum = sessions
                .iter()
                .map(|x| x.calc_point())
                .sum::<Result<u64>>()
                .unwrap();

            let msg = format!(
                "now <@!{}> has {} genkai point(+{}!)",
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