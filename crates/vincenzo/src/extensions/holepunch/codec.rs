use futures::SinkExt;
use speedy::{BigEndian, Readable};
use tokio::sync::oneshot;

use crate::{
    error::Error,
    extensions::{
        ExtData, ExtMsg, ExtMsgHandler, ExtendedMessage, Holepunch,
        HolepunchErrorCodes, HolepunchMsgType,
    },
    peer::MsgHandler,
    torrent::TorrentMsg,
};

#[derive(Debug, Clone)]
pub struct HolepunchCodec;

#[derive(Clone)]
pub struct HolepunchData();

impl ExtData for HolepunchData {}

impl TryFrom<ExtendedMessage> for Holepunch {
    type Error = Error;

    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != Self::ID {
            return Err(Error::PeerIdInvalid);
        }

        let holepunch =
            Holepunch::read_from_buffer_with_ctx(BigEndian {}, &value.1)?;

        Ok(holepunch)
    }
}

impl ExtMsgHandler<Holepunch, HolepunchData> for MsgHandler {
    async fn handle_msg(
        &self,
        peer: &mut crate::peer::Peer<crate::peer::Connected>,
        msg: Holepunch,
    ) -> Result<(), Error> {
        let remote_ext_id = 2;
        // let Some(remote_ext_id) = peer
        //     .state
        //     .ext_states
        //     .extension
        //     .as_ref()
        //     .and_then(|v| v.m.ut_holepunch)
        // else {
        //     return Ok(());
        // };

        match msg.msg_type {
            HolepunchMsgType::Rendezvous => {
                // 1. check if we have the target peer.
                // 2. send Connect msg to the src peer.
                // 3. send Connect msg to the target peer.

                let (otx, orx) = oneshot::channel();

                peer.state
                    .ctx
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::ReadPeerByIp(
                        msg.addr.into(),
                        msg.port,
                        otx,
                    ))
                    .await?;

                //
                // send connect to the src
                //
                let Some(peer_ctx) = orx.await? else {
                    peer.state
                        .sink
                        .send(
                            ExtendedMessage(
                                remote_ext_id,
                                msg.error(HolepunchErrorCodes::NotConnected)
                                    .try_into()?,
                            )
                            .into(),
                        )
                        .await?;
                    return Ok(());
                };

                // if the peer doesn't support the holepunch protocol.
                if peer.state.ext_states.holepunch.is_none() {
                    peer.state
                        .sink
                        .send(
                            ExtendedMessage(
                                remote_ext_id,
                                msg.error(HolepunchErrorCodes::NoSupport)
                                    .try_into()?,
                            )
                            .into(),
                        )
                        .await?;
                    return Ok(());
                }

                peer.state
                    .sink
                    .send(
                        ExtendedMessage(
                            remote_ext_id,
                            Holepunch::connect(
                                peer_ctx.remote_addr.into(),
                                peer_ctx.remote_addr.port(),
                            )
                            .try_into()?,
                        )
                        .into(),
                    )
                    .await?;
                //
                // send connect to the target
                //
                peer.state
                    .sink
                    .send(
                        ExtendedMessage(
                            remote_ext_id,
                            Holepunch::connect(
                                peer.state.ctx.remote_addr.into(),
                                peer.state.ctx.remote_addr.port(),
                            )
                            .try_into()?,
                        )
                        .into(),
                    )
                    .await?;
            }
            HolepunchMsgType::Connect => {
                // try to do handshake
            }
            HolepunchMsgType::Error => {
                //
            }
        }

        Ok(())
    }
}
