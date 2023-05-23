use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use collab::core::collab::MutexCollab;
use collab::core::origin::CollabOrigin;
use collab_sync::client::sink::{
  CollabSink, CollabSinkRunner, MsgId, MsgIdCounter, SinkConfig, SinkMessage,
};
use collab_sync::client::TokioUnboundedSink;
use parking_lot::Mutex;
use rand::Rng;
use tokio::spawn;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::watch;
use yrs::updates::decoder::Decode;
use yrs::{merge_updates_v1, ReadTxn, Update};

/// The [RemoteCollabStorage] is used to store the updates of the remote collab. The [RemoteCollab]
/// is the remote collab that maps to the local collab.
/// Any storage that implements this trait can be used as the remote collab storage.
#[async_trait]
pub trait RemoteCollabStorage: Send + Sync + 'static {
  /// Get all the updates of the remote collab.
  async fn get_all_updates(&self, object_id: &str) -> Result<Vec<Vec<u8>>, anyhow::Error>;
  /// Send the update to the remote storage.
  async fn send_update(&self, id: MsgId, update: Vec<u8>) -> Result<(), anyhow::Error>;
}

/// The [RemoteCollab] is used to sync the local collab to the remote.
pub struct RemoteCollab {
  object_id: String,
  collab: Arc<MutexCollab>,
  storage: Arc<dyn RemoteCollabStorage>,
  /// The [CollabSink] is used to send the updates to the remote.
  sink: Arc<CollabSink<TokioUnboundedSink<Message>, Message>>,
}

impl RemoteCollab {
  /// Create a new remote collab.
  /// `timeout` is the time to wait for the server to ack the message.
  /// If the server does not ack the message in time, the message will be sent again.
  pub fn new<S>(object_id: String, storage: S, config: SinkConfig) -> Self
  where
    S: RemoteCollabStorage + Send + Sync + 'static,
  {
    let storage: Arc<dyn RemoteCollabStorage> = Arc::new(storage);
    let collab = Arc::new(MutexCollab::new(CollabOrigin::Empty, &object_id, vec![]));
    let (sink, mut stream) = unbounded_channel::<Message>();
    let weak_storage = Arc::downgrade(&storage);
    let (notifier, notifier_rx) = watch::channel(false);
    let sink = Arc::new(CollabSink::new(
      TokioUnboundedSink(sink),
      notifier,
      RngMsgIdCounter::new(),
      config,
    ));

    let weak_sink = Arc::downgrade(&sink);
    spawn(async move {
      while let Some(message) = stream.recv().await {
        if let Some(storage) = weak_storage.upgrade() {
          if let Ok((msg_id, payload)) = message.into_payload() {
            match storage.send_update(msg_id, payload).await {
              Ok(_) => {
                tracing::debug!("ack update: {}", msg_id);
                if let Some(sink) = weak_sink.upgrade() {
                  sink.ack_msg(msg_id).await;
                }
              },
              Err(e) => {
                tracing::error!("send {} update failed: {:?}", msg_id, e);
              },
            }
          } else {
            tracing::error!("Failed to get the payload from message");
          }
        }
      }
    });

    spawn(CollabSinkRunner::run(Arc::downgrade(&sink), notifier_rx));
    Self {
      object_id,
      collab,
      storage,
      sink,
    }
  }

  pub async fn sync(&self, local_collab: Arc<MutexCollab>) {
    let updates = match self.storage.get_all_updates(&self.object_id).await {
      Ok(updates) => updates,
      Err(e) => {
        tracing::error!("🔴Failed to get updates: {:?}", e);
        vec![]
      },
    };

    if !updates.is_empty() {
      self.collab.lock().with_transact_mut(|txn| {
        for update in updates {
          if let Ok(update) = Update::decode_v1(&update) {
            txn.apply_update(update);
          } else {
            tracing::error!("Failed to decode update");
          }
        }
      });

      // Update local collab
      let local_sv = local_collab.lock().transact().state_vector();
      let encode_update = self
        .collab
        .lock()
        .transact()
        .encode_state_as_update_v1(&local_sv);
      if let Ok(update) = Update::decode_v1(&encode_update) {
        local_collab.lock().with_transact_mut(|txn| {
          txn.apply_update(update);
        });
      }
    }

    // Update remote collab
    let remote_state_vector = self.collab.lock().transact().state_vector();
    let encode_update = local_collab
      .lock()
      .transact()
      .encode_state_as_update_v1(&remote_state_vector);
    if let Ok(update) = Update::decode_v1(&encode_update) {
      self.collab.lock().with_transact_mut(|txn| {
        txn.apply_update(update);
      });
      self.push_update(&encode_update);
    }
  }

  pub fn push_update(&self, update: &[u8]) {
    self.sink.queue_or_merge_msg(
      |prev| {
        prev.merge_payload(update.to_vec());
        Ok(())
      },
      |msg_id| Message {
        object_id: self.object_id.clone(),
        msg_id,
        payloads: vec![update.to_vec()],
      },
    );
  }
}

#[derive(Clone, Debug)]
struct Message {
  object_id: String,
  msg_id: MsgId,
  payloads: Vec<Vec<u8>>,
}

impl Message {
  fn merge_payload(&mut self, payload: Vec<u8>) {
    self.payloads.push(payload);
  }

  fn payload_len(&self) -> usize {
    self.payloads.iter().map(|p| p.len()).sum()
  }

  fn into_payload(self) -> Result<(MsgId, Vec<u8>), anyhow::Error> {
    let updates = self
      .payloads
      .iter()
      .map(|update| update.as_ref())
      .collect::<Vec<&[u8]>>();
    let update = merge_updates_v1(&updates)?;
    Ok((self.msg_id, update))
  }
}

impl SinkMessage for Message {
  fn length(&self) -> usize {
    self.payload_len()
  }

  fn can_merge(&self) -> bool {
    self.payload_len() < 1024
  }
}

impl Eq for Message {}

impl PartialEq for Message {
  fn eq(&self, other: &Self) -> bool {
    self.msg_id == other.msg_id
  }
}

impl PartialOrd for Message {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Message {
  fn cmp(&self, other: &Self) -> Ordering {
    self.msg_id.cmp(&other.msg_id).reverse()
  }
}

impl Display for Message {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.write_fmt(format_args!(
      "send {} update: [msg_id:{}|payload_len:{}]",
      self.object_id,
      self.msg_id,
      self.payload_len(),
    ))
  }
}

#[derive(Debug, thiserror::Error)]
enum CollabError {
  #[error("Internal error")]
  Internal(#[from] anyhow::Error),
}

const RANDOM_MASK: u64 = (1 << 12) - 1;

struct RngMsgIdCounter(Mutex<MsgId>);

impl RngMsgIdCounter {
  pub fn new() -> Self {
    let timestamp = SystemTime::now()
      .duration_since(SystemTime::UNIX_EPOCH)
      .expect("Clock moved backwards!")
      .as_millis() as u64;

    let random: u64 = (rand::thread_rng().gen::<u16>() as u64) & RANDOM_MASK;
    let value = timestamp << 16 | random;
    Self(Mutex::new(value))
  }
}

impl MsgIdCounter for RngMsgIdCounter {
  fn next(&self) -> MsgId {
    let next = *self.0.lock() + 1;
    *self.0.lock() = next;
    next
  }
}
