//! Reine Warteschlangen-Logik (ohne Discord, ohne Audio) – Einreihen,
//! Obergrenzen, Bearbeiten per ID. Vollständig per Unit-Test prüfbar.

use std::collections::VecDeque;

use anyhow::{Result, bail};

use crate::model::{MAX_GUEST_QUEUED, MAX_QUEUED_REQUESTS, QueueEdit, QueueEntry};

/// Wo ein Wunsch eingereiht wird: vor dem ersten Autoplaylist-Eintrag,
/// sonst ans Ende.
pub(super) fn request_insert_index(queue: &VecDeque<QueueEntry>) -> usize {
    queue.iter().position(|e| e.autoplay).unwrap_or(queue.len())
}

/// Wie viele Tracks noch eingereiht werden dürfen: Wünsche insgesamt und –
/// bei Gästen – zusätzlich deren eigenes Kontingent. Autoplaylist-Einträge
/// zählen nicht mit.
pub(super) fn enqueue_room(queue: &VecDeque<QueueEntry>, guest: bool) -> usize {
    let requests = queue.iter().filter(|e| !e.autoplay).count();
    let room = MAX_QUEUED_REQUESTS.saturating_sub(requests);
    if !guest {
        return room;
    }
    let guests = queue.iter().filter(|e| e.guest).count();
    room.min(MAX_GUEST_QUEUED.saturating_sub(guests))
}

/// Höchstens so viele entfernte Einträge für „Rückgängig“ merken.
pub(super) const MAX_TRASH: usize = 2000;

/// Entfernt alle Einträge, für die `remove` gilt, und merkt sie sich
/// (mit alter Position) für „Rückgängig“.
pub(super) fn take_entries(
    queue: &mut VecDeque<QueueEntry>,
    trash: &mut Vec<(usize, QueueEntry)>,
    mut remove: impl FnMut(&QueueEntry) -> bool,
) -> usize {
    let mut removed = Vec::new();
    let mut kept = VecDeque::with_capacity(queue.len());
    for (i, e) in queue.drain(..).enumerate() {
        if remove(&e) {
            removed.push((i, e));
        } else {
            kept.push_back(e);
        }
    }
    *queue = kept;
    let n = removed.len();
    if n > 0 {
        removed.truncate(MAX_TRASH);
        *trash = removed;
    }
    n
}

/// Wendet eine Bearbeitung an; liefert die Zahl der betroffenen Einträge.
pub(super) fn apply_edit(
    queue: &mut VecDeque<QueueEntry>,
    trash: &mut Vec<(usize, QueueEntry)>,
    edit: QueueEdit,
) -> Result<usize> {
    use std::collections::HashSet;
    match edit {
        QueueEdit::Remove { ids } => {
            let ids: HashSet<u64> = ids.into_iter().collect();
            let n = take_entries(queue, trash, |e| ids.contains(&e.id));
            if n == 0 {
                bail!("Die Titel sind nicht mehr in der Warteschlange");
            }
            Ok(n)
        }
        QueueEdit::Move { ids, before, front } => {
            let set: HashSet<u64> = ids.into_iter().collect();
            if before.is_some_and(|b| set.contains(&b)) {
                bail!("Ungültiges Ziel");
            }
            if !queue.iter().any(|e| set.contains(&e.id)) {
                bail!("Die Titel sind nicht mehr in der Warteschlange");
            }
            if let Some(b) = before
                && !front
                && !queue.iter().any(|e| e.id == b)
            {
                bail!("Das Ziel ist nicht mehr in der Warteschlange – bitte neu laden");
            }
            let (moved, mut rest): (VecDeque<_>, VecDeque<_>) =
                queue.drain(..).partition(|e| set.contains(&e.id));
            let at = match (front, before) {
                (true, _) => 0,
                (false, Some(b)) => rest.iter().position(|e| e.id == b).unwrap_or(rest.len()),
                (false, None) => rest.len(),
            };
            let n = moved.len();
            for (offset, e) in moved.into_iter().enumerate() {
                rest.insert(at + offset, e);
            }
            *queue = rest;
            Ok(n)
        }
        QueueEdit::Restore => {
            if trash.is_empty() {
                bail!("Nichts zum Wiederherstellen");
            }
            let n = trash.len();
            for (i, e) in trash.drain(..) {
                let i = i.min(queue.len());
                queue.insert(i, e);
            }
            Ok(n)
        }
        QueueEdit::Dedupe => {
            let mut seen = HashSet::new();
            Ok(take_entries(queue, trash, |e| !seen.insert(e.meta.key())))
        }
        QueueEdit::RemoveAutoplay => Ok(take_entries(queue, trash, |e| e.autoplay)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lemusic_core::TrackMeta;

    use super::*;

    fn entry(id: u64, autoplay: bool) -> QueueEntry {
        QueueEntry {
            id,
            meta: Arc::new(TrackMeta {
                title: format!("T{id}"),
                author: String::new(),
                url: format!("https://youtu.be/{id}"),
                duration: None,
                thumbnail: None,
            }),
            requester: 0,
            autoplay,
            guest: false,
        }
    }

    #[test]
    fn enqueue_room_limits_requests_and_guests() {
        // Autoplaylist zählt nicht mit.
        let auto: VecDeque<_> = (0..3000).map(|i| entry(i, true)).collect();
        assert_eq!(enqueue_room(&auto, false), MAX_QUEUED_REQUESTS);
        assert_eq!(enqueue_room(&auto, true), MAX_GUEST_QUEUED);

        // Wünsche: Gesamtgrenze.
        let mut q: VecDeque<_> = (0..MAX_QUEUED_REQUESTS as u64 - 5)
            .map(|i| entry(i, false))
            .collect();
        assert_eq!(enqueue_room(&q, false), 5);
        assert_eq!(enqueue_room(&q, true), 5);

        // Gäste: eigenes Kontingent.
        q.clear();
        q.extend((0..MAX_GUEST_QUEUED as u64 - 2).map(|i| {
            let mut e = entry(i, false);
            e.guest = true;
            e
        }));
        assert_eq!(enqueue_room(&q, true), 2);
        assert_eq!(enqueue_room(&q, false), MAX_QUEUED_REQUESTS - q.len());
    }

    #[test]
    fn requests_go_before_autoplaylist() {
        // Nur Autoplaylist → Wunsch kommt als Nächstes.
        let only_auto: VecDeque<_> = [entry(1, true), entry(2, true)].into();
        assert_eq!(request_insert_index(&only_auto), 0);
        // Wünsche + Autoplaylist → hinter die Wünsche, vor die Autoplaylist.
        let mixed: VecDeque<_> = [entry(1, false), entry(2, false), entry(3, true)].into();
        assert_eq!(request_insert_index(&mixed), 2);
        // Keine Autoplaylist → ans Ende.
        let requests: VecDeque<_> = [entry(1, false)].into();
        assert_eq!(request_insert_index(&requests), 1);
        assert_eq!(request_insert_index(&VecDeque::new()), 0);
    }

    fn ids(q: &VecDeque<QueueEntry>) -> Vec<u64> {
        q.iter().map(|e| e.id).collect()
    }

    #[test]
    fn queue_edits_by_id() -> anyhow::Result<()> {
        let mut q: VecDeque<_> = (1..=5).map(|i| entry(i, i > 3)).collect();
        let mut trash = Vec::new();
        // 2 und 4 vor 1
        apply_edit(
            &mut q,
            &mut trash,
            QueueEdit::Move {
                ids: vec![4, 2],
                before: Some(1),
                front: false,
            },
        )?;
        assert_eq!(ids(&q), [2, 4, 1, 3, 5]);
        // 5 nach vorne, 2 ans Ende
        apply_edit(
            &mut q,
            &mut trash,
            QueueEdit::Move {
                ids: vec![5],
                before: None,
                front: true,
            },
        )?;
        apply_edit(
            &mut q,
            &mut trash,
            QueueEdit::Move {
                ids: vec![2],
                before: None,
                front: false,
            },
        )?;
        assert_eq!(ids(&q), [5, 4, 1, 3, 2]);
        // Ziel weg → Fehler, nichts verändert
        assert!(
            apply_edit(
                &mut q,
                &mut trash,
                QueueEdit::Move {
                    ids: vec![1],
                    before: Some(99),
                    front: false
                }
            )
            .is_err()
        );
        assert_eq!(ids(&q), [5, 4, 1, 3, 2]);
        // Entfernen + Rückgängig stellt Positionen wieder her
        assert_eq!(
            apply_edit(&mut q, &mut trash, QueueEdit::Remove { ids: vec![4, 3] })?,
            2
        );
        assert_eq!(ids(&q), [5, 1, 2]);
        apply_edit(&mut q, &mut trash, QueueEdit::Restore)?;
        assert_eq!(ids(&q), [5, 4, 1, 3, 2]);
        assert!(apply_edit(&mut q, &mut trash, QueueEdit::Restore).is_err());
        // Autoplaylist-Titel entfernen (4 und 5)
        assert_eq!(
            apply_edit(&mut q, &mut trash, QueueEdit::RemoveAutoplay)?,
            2
        );
        assert_eq!(ids(&q), [1, 3, 2]);
        Ok(())
    }

    #[test]
    fn dedupe_keeps_first() -> anyhow::Result<()> {
        let mut q: VecDeque<_> = [entry(1, false), entry(2, false), entry(1, true)]
            .into_iter()
            .enumerate()
            .map(|(i, mut e)| {
                e.id = i as u64 + 10;
                e
            })
            .collect();
        let mut trash = Vec::new();
        assert_eq!(apply_edit(&mut q, &mut trash, QueueEdit::Dedupe)?, 1);
        assert_eq!(ids(&q), [10, 11]);
        Ok(())
    }
}
