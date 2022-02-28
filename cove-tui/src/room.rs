use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use cove_core::conn::{self, ConnMaintenance, ConnRx, ConnTx};
use cove_core::packets::{
    Cmd, IdentifyCmd, IdentifyRpl, NickRpl, Ntf, Packet, RoomCmd, RoomRpl, Rpl, SendRpl, WhoRpl,
};
use cove_core::{Session, SessionId};
use tokio::sync::oneshot::{self, Sender};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tui::widgets::StatefulWidget;

use crate::config::Config;
use crate::never::Never;
use crate::replies::{self, Replies};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not connected")]
    NotConnected,
    #[error("not present")]
    NotPresent,
    #[error("incorrect reply type")]
    IncorrectReplyType,
    #[error("{0}")]
    Conn(#[from] conn::Error),
    #[error("{0}")]
    Replies(#[from] replies::Error),
}

pub enum StopReason {
    CouldNotConnect(conn::Error),
    InvalidRoom(String),
    InvalidIdentity(String),
    /// Something went wrong but we don't know what.
    SomethingWentWrong,
}

/// General state of the room connection.
pub enum Status {
    /// Connecting to the room for the first time.
    Connecting,
    /// Reconnecting to the room after being connected at least once.
    Reconnecting,
    /// Identifying with the server after a connection has been established.
    /// This occurs when an initial nick has been set on room creation.
    Identifying,
    /// User must enter a nick. May contain error message about previous nick.
    NickRequired(Option<String>),
    /// Fully connected.
    Nominal,
    /// Not connected and not attempting any reconnects. This is likely due to
    /// factors out of the application's control (e. g. no internet connection,
    /// room does not exist), meaning that retrying without user intervention
    /// doesn't make sense.
    Stopped(StopReason),
}

/// State for when a websocket connection exists.
struct Connected {
    tx: ConnTx,
    next_id: u64,
    replies: Replies<u64, Rpl>,
}

/// State for when a client has fully joined a room.
pub struct Present {
    session: Session,
    others: HashMap<SessionId, Session>,
}

pub struct RoomState {
    identity: String,
    initial_nick: Option<String>,
    status: Status,
    connected: Option<Connected>,
    present: Option<Present>,
}

impl RoomState {
    fn on_rpl(
        &mut self,
        id: u64,
        rpl: Rpl,
        room_verified: &mut Option<RoomVerified>,
    ) -> anyhow::Result<()> {
        match &rpl {
            Rpl::Room(RoomRpl::Success) => {
                *room_verified = Some(RoomVerified::Yes);
            }
            Rpl::Room(RoomRpl::InvalidRoom { reason }) => {
                self.status = Status::Stopped(StopReason::InvalidRoom(reason.clone()));
                anyhow::bail!("invalid room");
            }
            Rpl::Identify(IdentifyRpl::Success {
                you,
                others,
                last_message,
            }) => {
                let session = you.clone();
                let others = others
                    .iter()
                    .map(|session| (session.id, session.clone()))
                    .collect();
                self.present = Some(Present { session, others });
                // TODO Send last message to store
            }
            Rpl::Identify(IdentifyRpl::InvalidNick { .. }) => {}
            Rpl::Identify(IdentifyRpl::InvalidIdentity { .. }) => {}
            Rpl::Nick(NickRpl::Success { you }) => {
                if let Some(present) = &mut self.present {
                    present.session = you.clone();
                }
            }
            Rpl::Nick(NickRpl::InvalidNick { .. }) => {}
            Rpl::Send(SendRpl::Success { message }) => {
                // TODO Send message to store
            }
            Rpl::Send(SendRpl::InvalidContent { .. }) => {}
            Rpl::Who(WhoRpl { you, others }) => {
                if let Some(present) = &mut self.present {
                    present.session = you.clone();
                    present.others = others
                        .iter()
                        .map(|session| (session.id, session.clone()))
                        .collect();
                }
            }
        }

        if let Some(connected) = &mut self.connected {
            connected.replies.complete(&id, rpl);
        }

        Ok(())
    }

    fn on_ntf(&mut self, ntf: Ntf) {
        match ntf {
            Ntf::Join(join) => {
                if let Some(present) = &mut self.present {
                    present.others.insert(join.who.id, join.who);
                }
            }
            Ntf::Nick(nick) => {
                if let Some(present) = &mut self.present {
                    present.others.insert(nick.who.id, nick.who);
                }
            }
            Ntf::Part(part) => {
                if let Some(present) = &mut self.present {
                    present.others.remove(&part.who.id);
                }
            }
            Ntf::Send(_) => {
                // TODO Send message to store
            }
        }
    }

    async fn cmd<C, R>(state: &Mutex<RoomState>, cmd: C) -> Result<R, Error>
    where
        C: Into<Cmd>,
        Rpl: TryInto<R>,
    {
        let pending_reply = {
            let mut state = state.lock().await;
            let connected = state.connected.as_mut().ok_or(Error::NotConnected)?;

            let id = connected.next_id;
            connected.next_id += 1;

            let pending_reply = connected.replies.wait_for(id);
            connected.tx.send(&Packet::cmd(id, cmd.into()))?;
            pending_reply
        };

        let rpl = pending_reply.get().await?;
        let rpl_value = rpl.try_into().map_err(|_| Error::IncorrectReplyType)?;
        Ok(rpl_value)
    }

    async fn select_room_and_identify(
        state: Arc<Mutex<RoomState>>,
        name: String,
    ) -> Result<(), Error> {
        let result: RoomRpl = Self::cmd(&state, RoomCmd { name }).await?;
        match result {
            RoomRpl::Success => {}
            RoomRpl::InvalidRoom { reason } => {
                let mut state = state.lock().await;
                state.status = Status::Stopped(StopReason::InvalidRoom(reason));
                // FIXME This does not actually stop the room
                state.connected = None;
                return Ok(());
            }
        }

        let nick = {
            if let Some(nick) = &(state.lock().await).initial_nick {
                nick.clone()
            } else {
                return Ok(());
            }
        };
        Self::identify(&state, nick).await
    }

    async fn identify(state: &Mutex<Self>, nick: String) -> Result<(), Error> {
        let identity = state.lock().await.identity.clone();
        let result: IdentifyRpl = Self::cmd(state, IdentifyCmd { nick, identity }).await?;
        Ok(())
    }
}

pub struct Room {
    state: Arc<Mutex<RoomState>>,
    /// Once this is dropped, all other room-related tasks, connections and
    /// values are cleaned up.
    dead_mans_switch: Sender<Never>,
}

enum RoomVerified {
    Yes,
    No(StopReason),
}

impl Room {
    pub async fn new(
        config: &'static Config,
        name: String,
        identity: String,
        initial_nick: Option<String>,
    ) -> Self {
        let (tx, rx) = oneshot::channel();

        let room = Room {
            state: Arc::new(Mutex::new(RoomState {
                identity,
                initial_nick,
                status: Status::Connecting,
                connected: None,
                present: None,
            })),
            dead_mans_switch: tx,
        };

        let state_clone = room.state.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = rx => {} // Watch dead man's switch
                _ = Self::run(state_clone, config,name) => {}
            }
        });

        room
    }

    /// Background task to connect to a room and stay connected.
    async fn run(state: Arc<Mutex<RoomState>>, config: &'static Config, name: String) {
        // The room exists and we have successfully connected to it before
        let mut room_verified = None;

        loop {
            // Try to connect and run
            match Self::connect(&config.cove_url, config.timeout).await {
                Ok((tx, rx, mt)) => {
                    state.lock().await.connected = Some(Connected {
                        tx,
                        next_id: 0,
                        replies: Replies::new(config.timeout),
                    });

                    tokio::select! {
                        _ = mt.perform() => {}
                        _ = Self::receive(&state, rx, &mut room_verified) => {}
                    }
                }
                Err(e) if room_verified.is_none() => {
                    room_verified = Some(RoomVerified::No(StopReason::CouldNotConnect(e)))
                }
                Err(_) => {}
            }

            // Clean up and maybe reconnect
            {
                let mut state = state.lock().await;
                match room_verified {
                    Some(RoomVerified::Yes) => state.status = Status::Reconnecting,
                    Some(RoomVerified::No(reason)) => {
                        state.status = Status::Stopped(reason);
                        break;
                    }
                    None => {
                        state.status = Status::Stopped(StopReason::SomethingWentWrong);
                        break;
                    }
                }
            }
        }
    }

    async fn connect(
        url: &str,
        timeout: Duration,
    ) -> Result<(ConnTx, ConnRx, ConnMaintenance), conn::Error> {
        // This function exists to funnel errors using `?` short-circuiting.
        // Inlining it would be annoying and verbose.
        let stream = tokio_tungstenite::connect_async(url).await?.0;
        let conn = conn::new(stream, timeout)?;
        Ok(conn)
    }

    async fn receive(
        state: &Mutex<RoomState>,
        mut rx: ConnRx,
        room_verified: &mut Option<RoomVerified>,
    ) -> anyhow::Result<()> {
        while let Some(packet) = rx.recv().await? {
            match packet {
                Packet::Cmd { .. } => {} // Ignore, the server never sends commands
                Packet::Rpl { id, rpl } => {
                    state.lock().await.on_rpl(&room, id, rpl, room_verified)?;
                }
                Packet::Ntf { ntf } => room.lock().await.on_ntf(ntf),
            }
        }
        Ok(())
    }
}
