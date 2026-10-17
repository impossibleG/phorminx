//! Handshakes and the per-connection session loop.
//!
//! Every connection starts with the caller's `Hello`. A contacts service
//! answers only callers whose identity key belongs to a known, unblocked
//! contact; anyone else is disconnected without learning who we are. The
//! onion address authenticates the service, and the caller proves its
//! identity by signing a fresh challenge bound to that address.
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep, timeout};

use crate::engine::{Internal, PeerHello};
use crate::identity::{Identity, PublicKey, SecretBytes, random_bytes};
use crate::invite::{Invite, OnionAddress};
use crate::protocol::{
    CHALLENGE_BYTES, Frame, PROTOCOL_VERSION, ProtocolError, b64, key32, read_frame, unb64,
    write_frame,
};
use crate::transport::BoxStream;
use crate::{Config, MessagingError};

/// How long an invite service waits for the redeemer to hang up after replying.
const INVITE_LINGER: Duration = Duration::from_secs(30);

/// What a handshake or session task needs from the engine.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub identity: Arc<Identity>,
    pub display_name: String,
    pub actor: mpsc::UnboundedSender<Internal>,
    pub config: Arc<Config>,
}

pub(crate) enum SessionCmd {
    Send(Frame),
    Close,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HandshakeError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("peer refused")]
    Refused,
}

async fn expect_hello(stream: &mut BoxStream) -> Result<PeerHello, HandshakeError> {
    match read_frame(stream).await? {
        Frame::Hello {
            version,
            identity_key,
            display_name,
        } if version == PROTOCOL_VERSION => Ok(PeerHello {
            key: PublicKey(key32(&identity_key).ok_or(ProtocolError::Malformed)?),
            display_name,
        }),
        Frame::Hello { .. } => {
            let _ = write_frame(stream, &Frame::Goodbye {}).await;
            Err(HandshakeError::Refused)
        }
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

/// Server side: sends our hello and a challenge, then verifies the proof.
async fn challenge_caller(
    stream: &mut BoxStream,
    ctx: &Ctx,
    caller: &PublicKey,
    our_address: &OnionAddress,
) -> Result<(), HandshakeError> {
    write_frame(
        stream,
        &Frame::hello(&ctx.identity.public_key().0, &ctx.display_name),
    )
    .await?;
    let nonce = random_bytes::<CHALLENGE_BYTES>().map_err(|_| HandshakeError::Refused)?;
    write_frame(stream, &Frame::Challenge { nonce: b64(&nonce) }).await?;
    match read_frame(stream).await? {
        Frame::Proof { signature } => {
            let signature = unb64(&signature).ok_or(ProtocolError::Malformed)?;
            if caller.verify_proof(&nonce, &our_address.as_onion(), &signature) {
                Ok(())
            } else {
                Err(HandshakeError::Refused)
            }
        }
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

/// Client side: sends our hello, reads the service's hello and answers its
/// challenge.
async fn answer_challenge(
    stream: &mut BoxStream,
    ctx: &Ctx,
    dialed: &OnionAddress,
) -> Result<PeerHello, HandshakeError> {
    write_frame(
        stream,
        &Frame::hello(&ctx.identity.public_key().0, &ctx.display_name),
    )
    .await?;
    let peer = expect_hello(stream).await?;
    let nonce = match read_frame(stream).await? {
        Frame::Challenge { nonce } => unb64(&nonce).ok_or(ProtocolError::Malformed)?,
        Frame::Goodbye {} => return Err(HandshakeError::Refused),
        _ => return Err(ProtocolError::Unexpected.into()),
    };
    let proof = ctx.identity.prove(&nonce, &dialed.as_onion());
    write_frame(
        stream,
        &Frame::Proof {
            signature: b64(&proof),
        },
    )
    .await?;
    Ok(peer)
}

async fn ask<T>(
    ctx: &Ctx,
    make: impl FnOnce(oneshot::Sender<T>) -> Internal,
) -> Result<T, HandshakeError> {
    let (tx, rx) = oneshot::channel();
    ctx.actor
        .send(make(tx))
        .map_err(|_| HandshakeError::Refused)?;
    rx.await.map_err(|_| HandshakeError::Refused)
}

/// Accepts a caller on our contacts service.
pub(crate) async fn accept_contact(mut stream: BoxStream, ctx: Ctx, our_address: OnionAddress) {
    let result = timeout(ctx.config.handshake_timeout, async {
        let peer = expect_hello(&mut stream).await?;
        let contact_id = ask(&ctx, |reply| Internal::LookupContact {
            key: peer.key,
            reply,
        })
        .await?
        .ok_or(HandshakeError::Refused)?;
        challenge_caller(&mut stream, &ctx, &peer.key, &our_address).await?;
        Ok::<_, HandshakeError>((contact_id, peer))
    })
    .await;
    match result {
        Ok(Ok((contact_id, peer))) => {
            let _ = ctx.actor.send(Internal::PeerName {
                contact_id,
                display_name: peer.display_name,
            });
            run_session(stream, contact_id, ctx).await;
        }
        _ => {
            // Unknown, blocked or misbehaving callers get no explanation.
            let _ = stream.shutdown().await;
        }
    }
}

/// Dials a contact's contacts service.
pub(crate) async fn dial_contact(
    ctx: Ctx,
    connect: impl Future<Output = Result<BoxStream, crate::transport::TransportError>>,
    contact_id: i64,
    expected: PublicKey,
    address: OnionAddress,
) {
    let result = timeout(ctx.config.connect_timeout, async {
        let mut stream = connect.await.map_err(|_| HandshakeError::Refused)?;
        let peer = answer_challenge(&mut stream, &ctx, &address).await?;
        if peer.key != expected {
            return Err(HandshakeError::Refused);
        }
        Ok::<_, HandshakeError>((stream, peer))
    })
    .await;
    match result {
        Ok(Ok((stream, peer))) => {
            let _ = ctx.actor.send(Internal::PeerName {
                contact_id,
                display_name: peer.display_name,
            });
            run_session(stream, contact_id, ctx).await;
        }
        _ => {
            let _ = ctx.actor.send(Internal::DialFailed { contact_id });
        }
    }
}

/// Serves one invite redemption on an invite service.
pub(crate) async fn accept_invite(
    mut stream: BoxStream,
    ctx: Ctx,
    invite_id: String,
    invite_address: OnionAddress,
) {
    let finished_id = invite_id.clone();
    let replied = timeout(ctx.config.handshake_timeout, async {
        let peer = expect_hello(&mut stream).await?;
        challenge_caller(&mut stream, &ctx, &peer.key, &invite_address).await?;
        let (secret, contacts_address, discovery) = match read_frame(&mut stream).await? {
            Frame::RedeemInvite {
                secret,
                contacts_address,
                discovery_key,
            } => (
                unb64(&secret).ok_or(ProtocolError::Malformed)?,
                OnionAddress::parse(&contacts_address).ok_or(ProtocolError::Malformed)?,
                discovery_key
                    .as_deref()
                    .map(|k| key32(k).map(|k| SecretBytes::new(k.to_vec())))
                    .unwrap_or(None),
            ),
            _ => return Err(ProtocolError::Unexpected.into()),
        };
        let accepted = ask(&ctx, |reply| Internal::AcceptInvite {
            invite_id,
            secret,
            peer,
            contacts_address,
            discovery_secret: discovery,
            reply,
        })
        .await?;
        let frame = match accepted {
            Some((our_contacts_address, our_discovery)) => Frame::InviteAccepted {
                contacts_address: our_contacts_address.to_string(),
                discovery_key: our_discovery.map(|s| b64(s.as_bytes())),
            },
            None => Frame::InviteRejected {},
        };
        write_frame(&mut stream, &frame).await?;
        write_frame(&mut stream, &Frame::Goodbye {}).await?;
        Ok::<_, HandshakeError>(())
    })
    .await
    .is_ok_and(|result| result.is_ok());
    if replied {
        // Wait (bounded) for the redeemer to hang up. Tearing down the invite
        // service destroys its circuit, and Tor drops cells still queued on
        // it, so leaving first could lose the reply.
        let _ = timeout(INVITE_LINGER.min(ctx.config.handshake_timeout), async {
            while read_frame(&mut stream).await.is_ok() {}
        })
        .await;
    }
    let _ = stream.shutdown().await;
    // Only now may the engine tear down a used invite service: over Tor,
    // stopping it earlier closes the circuit before our reply is delivered.
    let _ = ctx.actor.send(Internal::InviteFinished {
        invite_id: finished_id,
    });
}

/// The result of redeeming someone's invite.
pub(crate) struct Redeemed {
    pub peer: PeerHello,
    pub contacts_address: OnionAddress,
    pub discovery_secret: Option<SecretBytes>,
}

/// Redeems an invite: connect, prove our key and the invite secret, and
/// exchange contacts addresses and discovery keys.
pub(crate) async fn redeem(
    ctx: Ctx,
    connect: impl Future<Output = Result<BoxStream, crate::transport::TransportError>>,
    invite: Invite,
    our_contacts_address: OnionAddress,
    our_discovery_secret: Option<SecretBytes>,
) -> Result<Redeemed, MessagingError> {
    let flow = async {
        let mut stream = connect
            .await
            .map_err(|_| MessagingError::InviteUnreachable)?;
        let peer = answer_challenge(&mut stream, &ctx, &invite.address)
            .await
            .map_err(|_| MessagingError::InviteUnreachable)?;
        if peer.key == ctx.identity.public_key() {
            return Err(MessagingError::OwnInvite);
        }
        write_frame(
            &mut stream,
            &Frame::RedeemInvite {
                secret: b64(&invite.secret),
                contacts_address: our_contacts_address.to_string(),
                discovery_key: our_discovery_secret.as_ref().map(|s| b64(s.as_bytes())),
            },
        )
        .await
        .map_err(|_| MessagingError::InviteUnreachable)?;
        let reply = read_frame(&mut stream)
            .await
            .map_err(|_| MessagingError::InviteRejected)?;
        let _ = stream.shutdown().await;
        match reply {
            Frame::InviteAccepted {
                contacts_address,
                discovery_key,
            } => Ok(Redeemed {
                peer,
                contacts_address: OnionAddress::parse(&contacts_address)
                    .ok_or(MessagingError::InviteRejected)?,
                discovery_secret: discovery_key
                    .as_deref()
                    .and_then(key32)
                    .map(|k| SecretBytes::new(k.to_vec())),
            }),
            _ => Err(MessagingError::InviteRejected),
        }
    };
    timeout(ctx.config.connect_timeout, flow)
        .await
        .map_err(|_| MessagingError::InviteUnreachable)?
}

/// Token bucket limiting how fast one contact can push messages at us.
pub(crate) struct RateLimit {
    tokens: f64,
    burst: f64,
    per_second: f64,
    last: Instant,
}

impl RateLimit {
    pub(crate) fn new(burst: u32, refill_every: Duration) -> Self {
        Self {
            tokens: f64::from(burst),
            burst: f64::from(burst),
            per_second: 1.0 / refill_every.as_secs_f64().max(0.001),
            last: Instant::now(),
        }
    }

    pub(crate) fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + (now - self.last).as_secs_f64() * self.per_second).min(self.burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Runs an authenticated session until either side leaves, the connection
/// drops, it idles out, or the peer breaks the protocol or the rate limit.
/// The rate limit is enforced by the engine per contact, across sessions.
pub(crate) async fn run_session(stream: BoxStream, contact_id: i64, ctx: Ctx) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<SessionCmd>(256);
    let (session_tx, session_rx) = oneshot::channel();
    if ctx
        .actor
        .send(Internal::SessionUp {
            contact_id,
            tx: cmd_tx,
            id_reply: session_tx,
        })
        .is_err()
    {
        return;
    }
    let Ok(session_id) = session_rx.await else {
        return;
    };
    let (reader, mut writer): (ReadHalf<BoxStream>, WriteHalf<BoxStream>) =
        tokio::io::split(stream);
    let (frame_tx, mut frame_rx) = mpsc::channel::<Frame>(16);
    let reader_task = tokio::spawn(read_loop(reader, frame_tx));
    let idle = sleep(ctx.config.idle_timeout);
    tokio::pin!(idle);
    loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                idle.as_mut().reset(Instant::now() + ctx.config.idle_timeout);
                match frame {
                    Some(Frame::Message { id, sent_at, text }) => {
                        let (tx, rx) = oneshot::channel();
                        let _ = ctx.actor.send(Internal::Incoming { contact_id, id: id.clone(), sent_at, text, reply: tx });
                        // The engine refuses messages over the contact's rate
                        // limit; leaving without an ack makes the sender back off.
                        match rx.await {
                            Ok(true) => {
                                if write_frame(&mut writer, &Frame::Ack { id }).await.is_err() {
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }
                    Some(Frame::Ack { id }) => {
                        let _ = ctx.actor.send(Internal::Acked { contact_id, id });
                    }
                    // Goodbye, a closed connection, or any frame that does not
                    // belong in an established session ends it.
                    _ => break,
                }
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(SessionCmd::Send(frame)) => {
                    idle.as_mut().reset(Instant::now() + ctx.config.idle_timeout);
                    if write_frame(&mut writer, &frame).await.is_err() {
                        break;
                    }
                }
                Some(SessionCmd::Close) | None => {
                    let _ = write_frame(&mut writer, &Frame::Goodbye {}).await;
                    break;
                }
            },
            _ = &mut idle => {
                let _ = write_frame(&mut writer, &Frame::Goodbye {}).await;
                break;
            }
        }
    }
    reader_task.abort();
    let _ = writer.shutdown().await;
    let _ = ctx.actor.send(Internal::SessionDown {
        contact_id,
        session_id,
    });
}

async fn read_loop(mut reader: ReadHalf<BoxStream>, frames: mpsc::Sender<Frame>) {
    while let Ok(frame) = read_frame(&mut reader).await {
        if frames.send(frame).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rate_limit_refills() {
        let mut rate = RateLimit::new(2, Duration::from_millis(20));
        assert!(rate.take());
        assert!(rate.take());
        assert!(!rate.take());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(rate.take());
    }
}
