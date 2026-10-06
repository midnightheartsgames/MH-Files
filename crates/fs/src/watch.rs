//! Наблюдатель за открытой папкой: события Windows склеиваются (~75 мс тишины), изменённые
//! пути перечитываются здесь же, в фоне, и в UI уходит готовая правка списка.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, unbounded};
use mh_files_platform::watch::{WatchEvent, Watcher};

use crate::{Event, Ticket, Workers, read};

const QUIET: Duration = Duration::from_millis(75);
/// Даже при непрерывном потоке событий (идёт копирование) правка уходит не реже этого.
const MAX_DELAY: Duration = Duration::from_millis(400);

/// Наблюдение идёт, пока объект жив.
pub struct DirWatch {
    _watcher: Watcher,
    pub dir: PathBuf,
}

pub(crate) fn start(workers: Workers, ticket: Ticket, dir: PathBuf) -> Option<DirWatch> {
    let (tx, rx) = unbounded::<Vec<WatchEvent>>();
    let watcher = Watcher::new(
        dir.clone(),
        Box::new(move |events| {
            let _ = tx.send(events);
        }),
    )
    .ok()?;
    // Поток живёт, пока жив отправитель внутри наблюдателя.
    workers.clone().spawn("watch", move |workers| {
        while let Ok(first) = rx.recv() {
            let mut events = first;
            let started = Instant::now();
            loop {
                match rx.recv_timeout(QUIET) {
                    Ok(more) => events.extend(more),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if started.elapsed() >= MAX_DELAY {
                    break;
                }
            }
            workers.send(patch(ticket, events));
        }
    });
    Some(DirWatch { _watcher: watcher, dir })
}

fn patch(ticket: Ticket, events: Vec<WatchEvent>) -> Event {
    let mut touched = BTreeSet::new();
    let mut reload = false;
    for event in events {
        match event {
            WatchEvent::Created(path) | WatchEvent::Modified(path) | WatchEvent::Removed(path) => {
                touched.insert(path);
            }
            WatchEvent::Renamed { from, to } => {
                touched.insert(from);
                touched.insert(to);
            }
            WatchEvent::Overflow | WatchEvent::Stopped => reload = true,
        }
    }
    if reload {
        return Event::Patch { ticket, upserts: Vec::new(), removes: Vec::new(), reload };
    }
    // Итог по каждому пути — по состоянию диска сейчас, а не по истории событий.
    let (mut upserts, mut removes) = (Vec::new(), Vec::new());
    for path in touched {
        match read::stat(&path) {
            Ok(mut entry) => {
                read::apply_dot_hidden(&mut entry);
                upserts.push(entry);
            }
            Err(_) => removes.push(path),
        }
    }
    Event::Patch { ticket, upserts, removes, reload: false }
}
