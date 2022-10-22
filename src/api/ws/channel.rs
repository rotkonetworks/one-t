// The MIT License (MIT)
// Copyright © 2021 Aukbit Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use crate::api::{
    responses::{
        AuthorityKey, AuthorityKeyCache, BlockResult, BlocksResult, CacheMap, ParachainsResult,
        SessionResult, ValidatorResult, ValidatorsResult,
    },
    ws::server::{Message, Remove, Server, WsResponseMessage},
};
use crate::cache::{create_or_await_pool, get_conn, CacheKey, Index, RedisPool, Verbosity};
use crate::config::CONFIG;
use crate::records::{BlockNumber, EpochIndex};

use actix::prelude::*;

use futures::executor::block_on;
use log::{info, warn};
use redis::aio::Connection;
use std::{collections::HashMap, time::Duration};
use subxt::sp_runtime::AccountId32;

const BLOCK_INTERVAL: Duration = Duration::from_secs(6);

#[derive(Clone, Eq, Hash, PartialEq, Debug)]
pub enum Topic {
    FinalizedBlock,
    BestBlock,
    NewSession,
    Validator(AccountId32),
    ParaAuthorities(EpochIndex, Verbosity),
    Parachains(EpochIndex),
}

impl std::fmt::Display for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FinalizedBlock => write!(f, "finalized_block"),
            Self::BestBlock => write!(f, "best_block"),
            Self::NewSession => write!(f, "new_session"),
            Self::Validator(account) => write!(f, "v:{}", account),
            Self::ParaAuthorities(index, verbosity) => write!(f, "pas:{}:{}", index, verbosity),
            Self::Parachains(index) => write!(f, "parachains:{}", index),
        }
    }
}

/// `Channel` manages topic subscriptions.
///
pub struct Channel {
    topic: Topic,
    sessions: HashMap<usize, Recipient<Message>>,
    cache: RedisPool,
    server_addr: Addr<Server>,
}

impl Channel {
    pub fn new(topic: Topic, addr: Addr<Server>) -> Channel {
        Channel {
            topic,
            sessions: HashMap::new(),
            cache: create_or_await_pool(CONFIG.clone()),
            server_addr: addr,
        }
    }
}

impl Channel {
    /// Publish message to all subscribers in the channel
    fn publish_message(&self, message: &str, skip_id: usize) {
        for (id, addr) in &self.sessions {
            if *id != skip_id {
                let _ = addr.do_send(Message(message.to_owned()));
            }
        }
    }

    fn reply_message(&self, id: usize, message: &str) {
        if let Some(addr) = self.sessions.get(&id) {
            let _ = addr.do_send(Message(message.to_owned()));
        }
    }
}

impl Channel {
    /// helper method that fetches data from cache and send it to subscribers at every block rate.
    fn run(&self, ctx: &mut Context<Self>) {
        ctx.run_interval(BLOCK_INTERVAL, |act, ctx| {
            // stop actor if no registered sessions
            if act.sessions.len() == 0 {
                ctx.stop();
                return;
            }

            // TODO handle all topics here
            match &act.topic {
                Topic::FinalizedBlock => {
                    let future = async {
                        if let Ok(mut conn) = get_conn(&act.cache).await {
                            if let Ok(finalized_block_number) = redis::cmd("GET")
                                .arg(CacheKey::FinalizedBlock)
                                .query_async::<Connection, BlockNumber>(&mut conn)
                                .await
                            {
                                if let Ok(pushed_block_number) = redis::cmd("GET")
                                    .arg(CacheKey::PushedBlock)
                                    .query_async::<Connection, BlockNumber>(&mut conn)
                                    .await
                                {
                                    // Note: if latest pushed block is different thant the finalized block oin cache
                                    // just send all finalized blocks not pushed to clinets yet.
                                    if pushed_block_number != finalized_block_number {
                                        let mut data: Vec<BlockResult> = Vec::new();

                                        let mut latest_block_pushed: Option<BlockNumber> =
                                            Some(pushed_block_number);

                                        while let Some(block_number) = latest_block_pushed {
                                            if finalized_block_number == block_number {
                                                latest_block_pushed = None;
                                            } else {
                                                if let Ok(serialized_data) = redis::cmd("GET")
                                                    .arg(CacheKey::BlockByIndexStats(Index::Num(
                                                        block_number.into(),
                                                    )))
                                                    .query_async::<Connection, String>(&mut conn)
                                                    .await
                                                {
                                                    let mut block_data = CacheMap::new();
                                                    block_data.insert(
                                                        String::from("block_number"),
                                                        block_number.to_string(),
                                                    );
                                                    block_data.insert(
                                                        String::from("is_finalized"),
                                                        (true).to_string(),
                                                    );
                                                    block_data.insert(
                                                        String::from("stats"),
                                                        serialized_data,
                                                    );

                                                    //
                                                    data.push(block_data.into());
                                                }
                                                //
                                                latest_block_pushed = Some(block_number + 1);
                                            }
                                        }

                                        let resp = WsResponseMessage {
                                            r#type: String::from("blocks"),
                                            result: BlocksResult::from(data),
                                        };
                                        let serialized = serde_json::to_string(&resp).unwrap();
                                        act.publish_message(&serialized, 0);

                                        // cache latest pushed block
                                        if let Err(e) = redis::cmd("SET")
                                            .arg(CacheKey::PushedBlock)
                                            .arg(finalized_block_number)
                                            .query_async::<Connection, String>(&mut conn)
                                            .await
                                        {
                                            warn!("Cache PushedBlock failed with error: {:?}", e);
                                        }
                                    }
                                } else {
                                    // first time just push to clients the last finalized block
                                    if let Ok(serialized_data) = redis::cmd("GET")
                                        .arg(CacheKey::BlockByIndexStats(Index::Num(
                                            finalized_block_number.into(),
                                        )))
                                        .query_async::<Connection, String>(&mut conn)
                                        .await
                                    {
                                        let mut block_data = CacheMap::new();
                                        block_data.insert(
                                            String::from("block_number"),
                                            finalized_block_number.to_string(),
                                        );
                                        block_data.insert(
                                            String::from("is_finalized"),
                                            (true).to_string(),
                                        );
                                        block_data.insert(String::from("stats"), serialized_data);

                                        let resp = WsResponseMessage {
                                            r#type: String::from("block"),
                                            result: BlockResult::from(block_data),
                                        };
                                        let serialized = serde_json::to_string(&resp).unwrap();
                                        act.publish_message(&serialized, 0);

                                        // cache pushed block
                                        if let Err(e) = redis::cmd("SET")
                                            .arg(CacheKey::PushedBlock)
                                            .arg(finalized_block_number)
                                            .query_async::<Connection, String>(&mut conn)
                                            .await
                                        {
                                            warn!("Cache PushedBlock failed with error: {:?}", e);
                                        }
                                    }
                                }
                            }
                        }
                    };
                    block_on(future);
                }
                Topic::BestBlock => {
                    let future = async {
                        if let Ok(mut conn) = get_conn(&act.cache).await {
                            if let Ok(block_number) = redis::cmd("GET")
                                .arg(CacheKey::BestBlock)
                                .query_async::<Connection, BlockNumber>(&mut conn)
                                .await
                            {
                                let mut block_data = CacheMap::new();
                                block_data
                                    .insert(String::from("block_number"), block_number.to_string());
                                block_data
                                    .insert(String::from("is_finalized"), (false).to_string());

                                let resp = WsResponseMessage {
                                    r#type: String::from("block"),
                                    result: BlockResult::from(block_data),
                                };
                                let serialized = serde_json::to_string(&resp).unwrap();
                                act.publish_message(&serialized, 0);
                            }
                        }
                    };
                    block_on(future);
                }
                Topic::NewSession => {
                    // TODO subscribe new session and send it over only when session changes
                    let future = async {
                        if let Ok(mut conn) = get_conn(&act.cache).await {
                            if let Ok(current) = redis::cmd("GET")
                                .arg(CacheKey::SessionByIndex(Index::Str(String::from(
                                    "current",
                                ))))
                                .query_async::<Connection, EpochIndex>(&mut conn)
                                .await
                            {
                                if let Ok(mut current_data) = redis::cmd("HGETALL")
                                    .arg(CacheKey::SessionByIndex(Index::Num(current.into())))
                                    .query_async::<Connection, CacheMap>(&mut conn)
                                    .await
                                {
                                    let zero = "0".to_string();
                                    let current_block = current_data
                                        .get("current_block")
                                        .unwrap_or(&zero)
                                        .parse::<BlockNumber>()
                                        .unwrap_or_default();
                                    let start_block = current_data
                                        .get("start_block")
                                        .unwrap_or(&zero)
                                        .parse::<BlockNumber>()
                                        .unwrap_or_default();
                                    let diff = current_block - start_block;

                                    // let's push the current_session to clients every
                                    // the first 10 blocks of each session
                                    if diff < 10 {
                                        // send previous session (clients might find it useful
                                        // since is no longer the current session)
                                        if let Ok(mut previous_data) = redis::cmd("HGETALL")
                                            .arg(CacheKey::SessionByIndex(Index::Num(
                                                (current - 1).into(),
                                            )))
                                            .query_async::<Connection, CacheMap>(&mut conn)
                                            .await
                                        {
                                            // set is_current to false and send previous session
                                            previous_data.insert(
                                                String::from("is_current"),
                                                (false).to_string(),
                                            );
                                            let resp = WsResponseMessage {
                                                r#type: String::from("session"),
                                                result: SessionResult::from(previous_data),
                                            };
                                            let serialized = serde_json::to_string(&resp).unwrap();
                                            act.publish_message(&serialized, 0);
                                        }
                                        // set is_current to true and send new session
                                        current_data
                                            .insert(String::from("is_current"), (true).to_string());
                                        let resp = WsResponseMessage {
                                            r#type: String::from("session"),
                                            result: SessionResult::from(current_data),
                                        };
                                        let serialized = serde_json::to_string(&resp).unwrap();
                                        act.publish_message(&serialized, 0);
                                    }
                                }
                            }
                        }
                    };
                    block_on(future);
                }
                Topic::Validator(account) => {
                    let future =
                        async {
                            if let Ok(mut conn) = get_conn(&act.cache).await {
                                if let Ok(finalized_block_number) = redis::cmd("GET")
                                    .arg(CacheKey::FinalizedBlock)
                                    .query_async::<Connection, BlockNumber>(&mut conn)
                                    .await
                                {
                                    if let Ok(pushed_block_number) = redis::cmd("GET")
                                        .arg(CacheKey::PushedBlock)
                                        .query_async::<Connection, BlockNumber>(&mut conn)
                                        .await
                                    {
                                        // Note: if latest pushed block equals finalized block in cache
                                        // just send latest cached data.
                                        if pushed_block_number == finalized_block_number {
                                            if let Ok(current_session) = redis::cmd("GET")
                                                .arg(CacheKey::SessionByIndex(Index::Str(
                                                    String::from("current"),
                                                )))
                                                .query_async::<Connection, EpochIndex>(&mut conn)
                                                .await
                                            {
                                                if let Ok(data) = redis::cmd("HGETALL")
                                                    .arg(CacheKey::AuthorityKeyByAccountAndSession(
                                                        account.clone(),
                                                        current_session,
                                                    ))
                                                    .query_async::<Connection, AuthorityKeyCache>(
                                                        &mut conn,
                                                    )
                                                    .await
                                                {
                                                    if !data.is_empty() {
                                                        let key: AuthorityKey = data.into();

                                                        if let Ok(mut data) = redis::cmd("HGETALL")
                                                            .arg(key.to_string())
                                                            .query_async::<Connection, CacheMap>(
                                                                &mut conn,
                                                            )
                                                            .await
                                                        {
                                                            if let Ok(tmp) = redis::cmd("HGETALL")
                                                .arg(CacheKey::AuthorityRecordVerbose(
                                                    key.to_string(),
                                                    Verbosity::Stats,
                                                ))
                                                .query_async::<Connection, CacheMap>(&mut conn)
                                                .await
                                            {
                                                data.extend(tmp);
                                            }
                                                            if let Ok(tmp) = redis::cmd("HGETALL")
                                                .arg(CacheKey::AuthorityRecordVerbose(
                                                    key.to_string(),
                                                    Verbosity::Summary,
                                                ))
                                                .query_async::<Connection, CacheMap>(&mut conn)
                                                .await
                                            {
                                                data.extend(tmp);
                                            }
                                                            data.insert(
                                                                String::from("session"),
                                                                current_session.to_string(),
                                                            );
                                                            let resp = WsResponseMessage {
                                                                r#type: String::from("validator"),
                                                                result: ValidatorResult::from(data),
                                                            };
                                                            let serialized =
                                                                serde_json::to_string(&resp)
                                                                    .unwrap();
                                                            act.publish_message(&serialized, 0);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        };
                    block_on(future);
                }
                Topic::ParaAuthorities(index, verbosity) => {
                    let future = async {
                        if let Ok(mut conn) = get_conn(&act.cache).await {
                            if let Ok(finalized_block_number) = redis::cmd("GET")
                                .arg(CacheKey::FinalizedBlock)
                                .query_async::<Connection, BlockNumber>(&mut conn)
                                .await
                            {
                                if let Ok(pushed_block_number) = redis::cmd("GET")
                                    .arg(CacheKey::PushedBlock)
                                    .query_async::<Connection, BlockNumber>(&mut conn)
                                    .await
                                {
                                    // Note: if latest pushed block equals finalized block in cache
                                    // just send latest cached data.
                                    if pushed_block_number == finalized_block_number {
                                        if let Ok(authority_keys) = redis::cmd("SMEMBERS")
                                            .arg(CacheKey::AuthorityKeysBySessionParaOnly(*index))
                                            .query_async::<Connection, Vec<String>>(&mut conn)
                                            .await
                                        {
                                            if !authority_keys.is_empty() {
                                                let mut data: Vec<ValidatorResult> = Vec::new();
                                                for key in authority_keys.iter() {
                                                    if let Ok(mut auth) = redis::cmd("HGETALL")
                                                        .arg(key)
                                                        .query_async::<Connection, CacheMap>(
                                                            &mut conn,
                                                        )
                                                        .await
                                                    {
                                                        if let Ok(tmp) = redis::cmd("HGETALL")
                                                            .arg(CacheKey::AuthorityRecordVerbose(
                                                                key.to_string(),
                                                                verbosity.clone(),
                                                            ))
                                                            .query_async::<Connection, CacheMap>(
                                                                &mut conn,
                                                            )
                                                            .await
                                                        {
                                                            auth.extend(tmp);
                                                        }
                                                        data.push(auth.into());
                                                    }
                                                }
                                                let resp = WsResponseMessage {
                                                    r#type: String::from("validators"),
                                                    result: ValidatorsResult {
                                                        session: *index,
                                                        data,
                                                    },
                                                };
                                                let serialized =
                                                    serde_json::to_string(&resp).unwrap();
                                                act.publish_message(&serialized, 0);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    block_on(future);
                }
                Topic::Parachains(index) => {
                    let future = async {
                        if let Ok(mut conn) = get_conn(&act.cache).await {
                            if let Ok(finalized_block_number) = redis::cmd("GET")
                                .arg(CacheKey::FinalizedBlock)
                                .query_async::<Connection, BlockNumber>(&mut conn)
                                .await
                            {
                                if let Ok(pushed_block_number) = redis::cmd("GET")
                                    .arg(CacheKey::PushedBlock)
                                    .query_async::<Connection, BlockNumber>(&mut conn)
                                    .await
                                {
                                    // Note: if latest pushed block equals finalized block in cache
                                    // just send latest cached data.
                                    if pushed_block_number == finalized_block_number {
                                        if let Ok(mut data) = redis::cmd("HGETALL")
                                            .arg(CacheKey::ParachainsBySession(*index))
                                            .query_async::<Connection, CacheMap>(&mut conn)
                                            .await
                                        {
                                            if !data.is_empty() {
                                                data.insert(
                                                    String::from("session"),
                                                    index.to_string(),
                                                );
                                                let resp = WsResponseMessage {
                                                    r#type: String::from("parachains"),
                                                    result: ParachainsResult::from(data),
                                                };
                                                let serialized =
                                                    serde_json::to_string(&resp).unwrap();
                                                act.publish_message(&serialized, 0);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    block_on(future);
                } // _ => (),
            }
        });
    }
}

/// Make actor from `Channel`
impl Actor for Channel {
    /// We are going to use simple Context, we just need ability to communicate
    /// with other actors.
    type Context = Context<Self>;

    /// Method is called on actor start.
    fn started(&mut self, ctx: &mut Context<Self>) {
        // start fetching data fro cache at every block rate
        self.run(ctx);
    }

    fn stopping(&mut self, _: &mut Context<Self>) -> Running {
        // notify server
        self.server_addr.do_send(Remove {
            topic: self.topic.clone(),
        });
        Running::Stop
    }
}

/// Subscribe to a topic, if channel for the topic does not exists create new channel.
#[derive(Message)]
#[rtype(result = "()")]
pub struct Subscribe {
    /// Client ID
    pub id: usize,

    /// Client Addr
    pub addr: Recipient<Message>,

    /// Topic
    pub topic: Topic,
}

/// Subscribe to a topic, remove client from old subscription with the same type
/// send successful subscription to the client
impl Handler<Subscribe> for Channel {
    type Result = ();

    fn handle(&mut self, msg: Subscribe, _ctx: &mut Context<Self>) {
        let Subscribe { id, addr, topic } = msg;

        info!("channel {} subscribed by session {}", topic, id);

        // add session to this channel
        self.sessions.entry(id).or_insert(addr);

        // build reply message
        let resp = WsResponseMessage {
            r#type: String::from("notifications"),
            result: format!("subscribed to {}", topic),
        };

        // serialize and send message only to the client
        if let Ok(serialized) = serde_json::to_string(&resp) {
            self.reply_message(id, &serialized);
        }
    }
}

/// Unsubscribe to a topic
#[derive(Message)]
#[rtype(result = "()")]
pub struct Unsubscribe {
    /// Client ID
    pub id: usize,
}

/// Unsubscribe to a topic, remove client from channel
impl Handler<Unsubscribe> for Channel {
    type Result = ();

    fn handle(&mut self, msg: Unsubscribe, _ctx: &mut Context<Self>) {
        let Unsubscribe { id } = msg;

        // remove address
        if self.sessions.remove(&msg.id).is_some() {
            info!("session {} unsubscribed from channel {}", id, self.topic);
        }
    }
}
