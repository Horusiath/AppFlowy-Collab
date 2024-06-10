use collab::core::collab::CollabReadOps;
use collab::preclude::Collab;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn awareness_insert_test() {
  let collab = Collab::new(1, "1", "1", vec![], true);
  let mut c = collab.write().await;
  c.emit_awareness_state();
  let (tx, rx) = mpsc::sync_channel(1);
  let _update = c.get_awareness().on_update(move |event| {
    tx.send(event.clone()).unwrap();
  });

  let s = json!({"name": "nathan"});
  c.get_mut_awareness().set_local_state(s.to_string());
  let state = c.get_awareness().local_state().unwrap();
  assert_eq!(state, s.to_string());

  sleep(Duration::from_secs(1)).await;
  let event = rx.recv().unwrap();
  assert_eq!(event.updated().len(), 1);
}

#[tokio::test]
async fn awareness_updates() {
  let c1 = Collab::new(1, "1", "1", vec![], true);
  let mut c1 = c1.write().await;
  c1.emit_awareness_state();
  let c2 = Collab::new(2, "1", "1", vec![], true);
  let mut c2 = c2.write().await;
  c2.emit_awareness_state();

  let sync = Arc::new(Mutex::new(None));
  let _u = {
    let ch = sync.clone();
    c1.get_awareness().on_update(move |event| {
      let update = event.awareness_state().full_update().unwrap();
      let mut lock = ch.lock().unwrap();
      *lock = Some(update);
    })
  };

  let s1 = json!({"name": "nathan"}).to_string();
  c1.get_mut_awareness().set_local_state(s1.clone());
  let s2 = json!({"name": "bartosz"}).to_string();
  c2.get_mut_awareness().set_local_state(s2.clone());
  let u2 = c2.get_awareness().update().unwrap();
  c1.get_mut_awareness().apply_update(u2).unwrap();

  let lock = sync.lock().unwrap();
  let awareness_update = lock.as_ref().unwrap();
  assert_eq!(awareness_update.clients.len(), 2);
  let values = awareness_update
    .clients
    .values()
    .map(|e| e.json.clone())
    .collect::<HashSet<_>>();
  assert_eq!(values, HashSet::from([s1, s2]));
}

#[tokio::test]
async fn initial_awareness_test() {
  let collab = Collab::new(1, "1", "1", vec![], true);
  let mut collab = collab.write().await;
  collab.emit_awareness_state();
  // by default, the awareness state contains the uid
  let state = collab.get_awareness().local_state().unwrap();
  assert_eq!(state, json!({"uid": 1}).to_string());
}

#[tokio::test]
async fn clean_awareness_state_test() {
  let collab = Collab::new(1, "1", "1", vec![], true);
  let mut collab = collab.write().await;
  collab.emit_awareness_state();
  let (tx, rx) = mpsc::sync_channel(1);
  let _update = collab.get_awareness().on_update(move |event| {
    tx.send(event.clone()).unwrap();
  });
  collab.clean_awareness_state();
  let event = rx.recv().unwrap();
  assert_eq!(event.removed().len(), 1);

  assert!(collab.get_awareness().local_state().is_none());
}

#[tokio::test]
async fn clean_awareness_state_sync_test() {
  let mut doc_id_map_uid = HashMap::new();
  let collab_1 = Collab::new(0, "1", "1", vec![], true);
  let mut collab_1 = collab_1.write().await;
  collab_1.emit_awareness_state();
  doc_id_map_uid.insert(collab_1.client_id(), 0.to_string());
  let (tx, rx) = mpsc::sync_channel(1);
  let _update = collab_1.get_awareness().on_update(move |event| {
    let update = event.awareness_update().unwrap();
    tx.send(update.clone()).unwrap();
  });
  collab_1.emit_awareness_state();

  // apply the awareness state from collab_a to collab_b
  let awareness_update = rx.recv().unwrap();
  let collab_2 = Collab::new(1, "1", "2", vec![], true);
  let mut collab_2 = collab_2.write().await;
  collab_2.emit_awareness_state();
  doc_id_map_uid.insert(collab_2.client_id(), 1.to_string());
  collab_2
    .get_mut_awareness()
    .apply_update(awareness_update.clone())
    .unwrap();

  // collab_a's awareness state should be synced to collab_b after applying the update
  let states = collab_2.get_awareness().clients();
  assert_eq!(states.len(), 2);
  for (id, json) in states {
    let uid = doc_id_map_uid.get(id).unwrap().parse::<i64>().unwrap();
    assert_eq!(json, &json!({"uid": uid}).to_string());
  }

  // collab_a clean the awareness state
  collab_1.clean_awareness_state();
  let awareness_update = rx.recv().unwrap();
  // apply the awareness state from collab_a to collab_b. collab_b's awareness state should be cleaned
  collab_2
    .get_mut_awareness()
    .apply_update(awareness_update.clone())
    .unwrap();

  let states = collab_2.get_awareness().clients();
  assert_eq!(states.len(), 1);
}
