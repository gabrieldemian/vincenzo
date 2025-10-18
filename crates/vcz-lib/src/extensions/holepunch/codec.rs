use crate::{
    error::Error,
    extensions::{
        ExtMsg, ExtMsgHandler, ExtendedMessage, Holepunch, HolepunchMsgType,
    },
    peer::{self, Peer},
    torrent::TorrentMsg,
};
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct HolepunchCodec;

impl TryFrom<ExtendedMessage> for Holepunch {
    type Error = Error;

    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != Self::ID {
            return Err(crate::error::Error::WrongExtensionId {
                local: Self::ID,
                received: value.0,
            });
        }

        let _holepunch = Holepunch::deserialize(&value.1)?;

        todo!();
    }
}

impl ExtMsgHandler<Holepunch> for Peer<peer::Connected> {
    type Error = Error;

    async fn handle_msg(&mut self, msg: Holepunch) -> Result<(), Error> {
        let Some(_remote_ext_id) = self.state.extension.as_ref() else {
            return Ok(());
        };

        match msg.msg_type {
            HolepunchMsgType::Rendezvous => {
                // 1. check if we have the target peer.
                // 2. send Connect msg to the src peer.
                // 3. send Connect msg to the target peer.

                let (otx, orx) = oneshot::channel();

                self.state
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
                let Some(_peer_ctx) = orx.await? else {
                    // peer.state
                    //     .sink
                    //     .send(
                    //         ExtendedMessage(
                    //             remote_ext_id,
                    //             msg.error(HolepunchErrorCodes::NotConnected)
                    //                 .try_into()?,
                    //         )
                    //         .into(),
                    //     )
                    //     .await?;
                    return Ok(());
                };

                // if the peer doesn't support the holepunch protocol.
                // if peer.state.ext_states.holepunch.is_none() {
                // peer.state
                //     .sink
                //     .send(
                //         ExtendedMessage(
                //             remote_ext_id,
                //             msg.error(HolepunchErrorCodes::NoSupport)
                //                 .try_into()?,
                //         )
                //         .into(),
                //     )
                //     .await?;
                // return Ok(());
                // }

                // peer.state
                //     .sink
                //     .send(
                //         ExtendedMessage(
                //             remote_ext_id,
                //             Holepunch::connect(
                //                 peer_ctx.remote_addr.into(),
                //                 peer_ctx.remote_addr.port(),
                //             )
                //             .try_into()?,
                //         )
                //         .into(),
                //     )
                //     .await?;
                //
                // send connect to the target
                //
                // peer.state
                //     .sink
                //     .send(
                //         ExtendedMessage(
                //             remote_ext_id,
                //             Holepunch::connect(
                //                 peer.state.ctx.remote_addr.into(),
                //                 peer.state.ctx.remote_addr.port(),
                //             )
                //             .try_into()?,
                //         )
                //         .into(),
                //     )
                //     .await?;
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
