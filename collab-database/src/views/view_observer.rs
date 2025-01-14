use collab::core::any_map::AnyMap;
use collab::preclude::array::ArrayEvent;
use collab::preclude::map::MapEvent;
use collab::preclude::{Change, TransactionMut};
use collab::preclude::{
  DeepEventsSubscription, DeepObservable, EntryChange, Event, MapRefWrapper, PathSegment,
};
use std::ops::Deref;
use std::str::FromStr;
use tokio::sync::broadcast;
use tracing::{trace, warn};

use crate::views::define::*;
use crate::views::{
  row_order_from_value, view_from_map_ref, view_from_value, view_id_from_map_ref, DatabaseLayout,
  DatabaseView, FieldOrder, FilterMap, GroupMap, RowOrder, SortMap,
};
#[derive(Debug, Clone)]
pub enum DatabaseViewChange {
  DidCreateView {
    view: DatabaseView,
  },
  DidUpdateView {
    view: DatabaseView,
  },
  DidDeleteView {
    view_id: String,
  },
  LayoutSettingChanged {
    view_id: String,
    layout_type: DatabaseLayout,
  },
  DidInsertRowOrders {
    row_orders: Vec<RowOrder>,
  },
  DidDeleteRowAtIndex {
    index: Vec<u32>,
  },
  // filter
  DidCreateFilters {
    view_id: String,
    filters: Vec<FilterMap>,
  },
  DidUpdateFilter {
    view_id: String,
  },
  // group
  DidCreateGroupSettings {
    view_id: String,
    groups: Vec<GroupMap>,
  },
  DidUpdateGroupSetting {
    view_id: String,
  },
  // Sort
  DidCreateSorts {
    view_id: String,
    sorts: Vec<SortMap>,
  },
  DidUpdateSort {
    view_id: String,
  },
  // field order
  DidCreateFieldOrder {
    view_id: String,
    field_order: FieldOrder,
  },
  DidDeleteFieldOrder {
    view_id: String,
    field_order: FieldOrder,
  },
}

pub type ViewChangeSender = broadcast::Sender<DatabaseViewChange>;
pub type ViewChangeReceiver = broadcast::Receiver<DatabaseViewChange>;

pub(crate) fn subscribe_view_map_change(
  view_map: &mut MapRefWrapper,
  change_tx: ViewChangeSender,
) -> DeepEventsSubscription {
  view_map.observe_deep(move |txn, events| {
    for event in events.iter() {
      match event {
        Event::Text(_) => {},
        Event::Array(array_event) => {
          handle_array_event(&change_tx, txn, array_event);
        },
        Event::Map(event) => {
          handle_map_event(&change_tx, txn, event);
        },
        Event::XmlFragment(_) => {},
        Event::XmlText(_) => {},
      }
    }
  })
}

/// Handles an array modification process consisting of retain and remove operations.
///
/// # Process
/// 1. Initial Array State:
///    - Starts with the array `[A B C]`.
///    - Offset is initially at position 0.
///
/// 2. Retain Operation:
///    - Retain 1: Retains the first element (`A`), moving the offset to the next element.
///    - After operation: `[A B C]`
///    - Offset is now at position 1 (pointing to `B`).
///
/// 3. Remove Operation:
///    - Remove 1: Removes one element at the current offset.
///    - `B` (at offset position 1) is removed from the array.
///    - After operation: `[A   C]`
///    - Offset remains at position 1.
///
/// 4. Final Array State:
///    - Resulting array after the remove operation: `[A C]`
///    - This reflects the removal of `B` from the original array.

fn handle_array_event(
  change_tx: &ViewChangeSender,
  txn: &TransactionMut,
  array_event: &ArrayEvent,
) {
  let mut offset = 0;
  let key = ArrayChangeKey::from(array_event);
  let mut deleted_row_index: Vec<u32> = vec![];
  array_event.delta(txn).iter().for_each(|change| {
    trace!("database view observe array event: {:?}:{:?}", key, change);
    match change {
      Change::Added(values) => match &key {
        ArrayChangeKey::RowOrder => {
          let row_orders = values
            .iter()
            .flat_map(|value| row_order_from_value(value, txn))
            .collect::<Vec<_>>();
          let _ = change_tx.send(DatabaseViewChange::DidInsertRowOrders { row_orders });
        },
        ArrayChangeKey::Filter => {
          if let Some(view_id) = view_id_from_array_event(array_event) {
            let filters = values
              .iter()
              .flat_map(|value| AnyMap::from_value(txn, value))
              .collect::<Vec<_>>();
            let _ = change_tx.send(DatabaseViewChange::DidCreateFilters { view_id, filters });
          }
        },
        ArrayChangeKey::Sort => {
          if let Some(view_id) = view_id_from_array_event(array_event) {
            let sorts = values
              .iter()
              .flat_map(|value| AnyMap::from_value(txn, value))
              .collect::<Vec<_>>();
            let _ = change_tx.send(DatabaseViewChange::DidCreateSorts { view_id, sorts });
          }
        },
        ArrayChangeKey::Group => {
          if let Some(view_id) = view_id_from_array_event(array_event) {
            let groups = values
              .iter()
              .flat_map(|value| AnyMap::from_value(txn, value))
              .collect::<Vec<_>>();
            let _ = change_tx.send(DatabaseViewChange::DidCreateGroupSettings { view_id, groups });
          }
        },
        ArrayChangeKey::Unhandled(s) => {
          trace!("database view observe unknown insert: {}", s);
        },
      },
      Change::Removed(len) => {
        // https://github.com/y-crdt/y-crdt/issues/341
        trace!("database view observe array remove: {}", len);
        match &key {
          ArrayChangeKey::RowOrder => {
            if *len > 0 {
              deleted_row_index.extend((offset..=(offset + len - 1)).collect::<Vec<_>>());
            }
            offset += len;
          },
          ArrayChangeKey::Filter => {
            if let Some(view_id) = view_id_from_array_event(array_event) {
              let _ = change_tx.send(DatabaseViewChange::DidUpdateFilter { view_id });
            }
          },
          ArrayChangeKey::Sort => {
            if let Some(view_id) = view_id_from_array_event(array_event) {
              let _ = change_tx.send(DatabaseViewChange::DidUpdateSort { view_id });
            }
          },
          ArrayChangeKey::Group => {
            if let Some(view_id) = view_id_from_array_event(array_event) {
              let _ = change_tx.send(DatabaseViewChange::DidUpdateGroupSetting { view_id });
            }
          },
          ArrayChangeKey::Unhandled(s) => {
            trace!("database view observe unknown remove: {}", s);
          },
        }
      },
      Change::Retain(value) => {
        offset += value;
        trace!("database view observe array retain: {}", value);
      },
    }
  });

  if !deleted_row_index.is_empty() {
    let _ = change_tx.send(DatabaseViewChange::DidDeleteRowAtIndex {
      index: deleted_row_index,
    });
  }
}

fn handle_map_event(change_tx: &ViewChangeSender, txn: &TransactionMut, event: &MapEvent) {
  let keys = event.keys(txn);
  for (key, value) in keys.iter() {
    let _change_tx = change_tx.clone();
    match value {
      EntryChange::Inserted(value) => {
        let database_view = view_from_value(value, txn);
        // trace!("database view map inserted: {}:{:?}", key, database_view,);
        if let Some(database_view) = database_view {
          let _ = change_tx.send(DatabaseViewChange::DidCreateView {
            view: database_view,
          });
        }
      },
      EntryChange::Updated(_, value) => {
        let database_view = view_from_map_ref(event.target(), txn);
        if let Some(database_view) = database_view {
          let _ = change_tx.send(DatabaseViewChange::DidUpdateView {
            view: database_view,
          });
        }

        let view_id = view_id_from_map_ref(event.target(), txn);
        trace!("database view map update: {}:{}", key, value);
        match (*key).as_ref() {
          DATABASE_VIEW_LAYOUT => {
            if let Ok(layout_type) = DatabaseLayout::from_str(&value.to_string()) {
              let _ = change_tx.send(DatabaseViewChange::LayoutSettingChanged {
                view_id,
                layout_type,
              });
            }
          },
          _ => {
            trace!("database view map update: {}:{}", key, value);
          },
        }
      },
      EntryChange::Removed(_value) => {
        // trace!("database view map delete: {}:{}", key, value);
        let view_id = (**key).to_string();
        if !view_id.is_empty() {
          let _ = change_tx.send(DatabaseViewChange::DidDeleteView { view_id });
        } else {
          warn!("database view map delete: empty key");
        }
      },
    }
  }
}

#[derive(Debug)]
enum ArrayChangeKey {
  Unhandled(String),
  RowOrder,
  Filter,
  Sort,
  Group,
}

impl From<&ArrayEvent> for ArrayChangeKey {
  fn from(event: &ArrayEvent) -> Self {
    match event.path().pop_back() {
      Some(segment) => match segment {
        PathSegment::Key(s) => match s.as_ref() {
          DATABASE_VIEW_ROW_ORDERS => Self::RowOrder,
          DATABASE_VIEW_FILTERS => Self::Filter,
          DATABASE_VIEW_SORTS => Self::Sort,
          DATABASE_VIEW_GROUPS => Self::Group,
          _ => Self::Unhandled(s.deref().to_string()),
        },
        PathSegment::Index(_) => Self::Unhandled("index".to_string()),
      },
      None => Self::Unhandled("empty path".to_string()),
    }
  }
}

fn view_id_from_array_event(event: &ArrayEvent) -> Option<String> {
  let path = event.path();
  if path.len() > 1 {
    match path.front() {
      Some(PathSegment::Key(key)) => Some(key.to_string()),
      _ => None,
    }
  } else {
    None
  }
}
