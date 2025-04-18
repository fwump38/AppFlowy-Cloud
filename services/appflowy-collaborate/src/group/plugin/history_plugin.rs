use collab::core::collab_plugin::CollabPluginType;
use collab::lock::RwLock;
use collab::preclude::{Collab, CollabPlugin};
use collab_entity::CollabType;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use tokio::time::sleep;
use tracing::{error, trace};
use uuid::Uuid;
use yrs::TransactionMut;

use database::collab::CollabStorage;
use database_entity::dto::InsertSnapshotParams;

/// [HistoryPlugin] will be moved to history collab server. For now, it's temporarily placed here.
pub struct HistoryPlugin<S> {
  workspace_id: Uuid,
  object_id: Uuid,
  collab_type: CollabType,
  storage: Arc<S>,
  did_create_snapshot: AtomicBool,
  weak_collab: Weak<RwLock<Collab>>,
  edit_count: AtomicU32,
  is_new_collab: bool,
}

impl<S> HistoryPlugin<S>
where
  S: CollabStorage,
{
  #[allow(dead_code)]
  pub fn new(
    workspace_id: Uuid,
    object_id: Uuid,
    collab_type: CollabType,
    weak_collab: Weak<RwLock<Collab>>,
    storage: Arc<S>,
    is_new_collab: bool,
  ) -> Self {
    Self {
      workspace_id,
      object_id,
      collab_type,
      storage,
      did_create_snapshot: Default::default(),
      weak_collab,
      edit_count: Default::default(),
      is_new_collab,
    }
  }

  async fn enqueue_snapshot(
    weak_collab: Weak<RwLock<Collab>>,
    storage: Arc<S>,
    workspace_id: Uuid,
    object_id: Uuid,
    collab_type: CollabType,
  ) -> Result<(), anyhow::Error> {
    trace!("trying to enqueue snapshot for object_id: {}", object_id);
    // Attempt to encode collaboration data for snapshot
    if let Some(collab) = weak_collab.upgrade() {
      let lock = collab.read().await;
      let encode_collab =
        lock.encode_collab_v1(|collab| collab_type.validate_require_data(collab))?;
      let data = encode_collab.doc_state;
      let params = InsertSnapshotParams {
        object_id,
        doc_state: data,
        workspace_id,
        collab_type,
      };
      storage.queue_snapshot(params).await?;
      trace!("successfully enqueued snapshot creation")
    }
    Ok(())
  }
}

impl<S> CollabPlugin for HistoryPlugin<S>
where
  S: CollabStorage,
{
  fn receive_update(&self, _object_id: &str, _txn: &TransactionMut, _update: &[u8]) {
    if self.is_new_collab {
      trace!("skip snapshot creation for new collab");
      return;
    }

    let old = self.edit_count.fetch_add(1, Ordering::SeqCst);
    if old < 20 {
      return;
    }

    if self.did_create_snapshot.load(Ordering::Relaxed) {
      return;
    }
    self.did_create_snapshot.store(true, Ordering::SeqCst);
    let storage = self.storage.clone();
    let weak_collab = self.weak_collab.clone();
    let collab_type = self.collab_type;

    let workspace_id = self.workspace_id;
    let object_id = self.object_id;
    tokio::spawn(async move {
      sleep(std::time::Duration::from_secs(2)).await;
      match storage
        .should_create_snapshot(&workspace_id, &object_id)
        .await
      {
        Ok(true) => {
          if let Err(err) =
            Self::enqueue_snapshot(weak_collab, storage, workspace_id, object_id, collab_type).await
          {
            error!("Failed to enqueue snapshot: {:?}", err);
          }
        },
        Ok(false) => { /* ignore */ },
        Err(err) => {
          trace!("Failed to check if snapshot should be created: {:?}", err);
        },
      }
    });
  }

  fn plugin_type(&self) -> CollabPluginType {
    CollabPluginType::Other("history".to_string())
  }
}
