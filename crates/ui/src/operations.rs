//! Файловые операции глазами интерфейса: очередь с прогрессом, пауза и отмена, проверка
//! конфликтов перед копированием, журнал «Отменить».

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use mh_files_core::rename::Plan;
use mh_files_fs::{Conflict, ShellJob, Ticket, Transfer};
use mh_files_platform::ops::{FileOp, OnConflict, OpEvent, OpId, OpOutcome, Progress};

use crate::app::{FilesApp, Level, OWNER_BATCH};
use crate::dialogs::Dialog;
use crate::tabs::InlineRename;

/// Что сделать после операции.
#[derive(Debug, Clone)]
pub enum Followup {
    /// Выделить созданное и сразу переименовать (новая папка).
    RenameCreated { tab: u64 },
    /// Выделить результат (переименование).
    SelectCreated { tab: u64 },
    /// Вставка вырезанного: очистить буфер обмена после успеха.
    ClearClipboard,
}

/// Операция в панели операций.
#[derive(Debug, Clone)]
pub struct OpView {
    pub id: OpId,
    pub label: String,
    pub running: bool,
    pub progress: Progress,
}

/// Чем отменяется сделанное.
#[derive(Debug, Clone)]
pub enum UndoEntry {
    Ops(Vec<FileOp>),
    /// Пакетное переименование отменяется обратным планом.
    Rename(Plan),
    /// Сортировщик отменяется по своему журналу.
    Sort(std::path::PathBuf),
}

/// Сколько шагов помнит «Отменить».
const JOURNAL_LIMIT: usize = 32;

#[derive(Default)]
pub struct Operations {
    pub list: Vec<OpView>,
    followups: HashMap<OpId, Followup>,
    /// Операции, которые сами отменяют прежние: в журнал не пишутся.
    undoing: HashSet<OpId>,
    journal: Vec<(String, UndoEntry)>,
    /// Переносы, ждущие проверки конфликтов.
    pending: Vec<(Transfer, Option<Followup>)>,
    /// Запущенное пакетное переименование — чтобы записать обратный план.
    pub batch: Option<Plan>,
}

impl Operations {
    pub fn is_busy(&self) -> bool {
        !self.list.is_empty()
    }

    /// Подпись следующего шага «Отменить».
    pub fn undo_label(&self) -> Option<&str> {
        self.journal.last().map(|(label, _)| label.as_str())
    }

    pub fn record_sort(&mut self, label: String, journal: std::path::PathBuf) {
        self.record(label, UndoEntry::Sort(journal));
    }

    fn record(&mut self, label: String, entry: UndoEntry) {
        self.journal.push((label, entry));
        if self.journal.len() > JOURNAL_LIMIT {
            self.journal.remove(0);
        }
    }
}

impl FilesApp {
    /// Поставить операцию в очередь.
    pub fn submit(&mut self, op: FileOp, followup: Option<Followup>) -> OpId {
        let label = op.describe();
        let id = self.ops.submit(op);
        self.operations.list.push(OpView {
            id,
            label,
            running: false,
            progress: Progress::default(),
        });
        if let Some(followup) = followup {
            self.operations.followups.insert(id, followup);
        }
        id
    }

    /// Копирование или перемещение: сначала проверка конфликтов в фоне, потом — свой диалог
    /// или сразу операция.
    pub fn start_transfer(&mut self, transfer: Transfer, followup: Option<Followup>) {
        self.operations.pending.push((transfer.clone(), followup));
        self.workers.preflight(transfer);
    }

    pub fn on_preflight(&mut self, transfer: Transfer, conflicts: Vec<Conflict>) {
        let Some(index) = self.operations.pending.iter().position(|(t, _)| *t == transfer) else {
            return;
        };
        let (transfer, followup) = self.operations.pending.remove(index);
        if conflicts.is_empty() {
            self.submit_transfer(&transfer, transfer.sources.clone(), OnConflict::Ask, followup);
        } else {
            self.dialog = Some(Dialog::conflicts(transfer, conflicts, followup));
        }
    }

    /// Решения из диалога конфликтов: объекты делятся на группы по решению, каждая группа —
    /// своя операция. Объекты без конфликта идут с первой группой.
    pub fn resolve_conflicts(
        &mut self,
        transfer: Transfer,
        decisions: Vec<(Conflict, OnConflict)>,
        followup: Option<Followup>,
    ) {
        let conflicted: HashSet<&PathBuf> = decisions.iter().map(|(c, _)| &c.source).collect();
        let mut free: Vec<PathBuf> =
            transfer.sources.iter().filter(|s| !conflicted.contains(s)).cloned().collect();
        let mut groups: Vec<(OnConflict, Vec<PathBuf>)> = Vec::new();
        for policy in [OnConflict::Replace, OnConflict::KeepBoth] {
            let sources: Vec<PathBuf> = decisions
                .iter()
                .filter(|(_, decision)| *decision == policy)
                .map(|(c, _)| c.source.clone())
                .collect();
            if !sources.is_empty() {
                groups.push((policy, sources));
            }
        }
        match groups.first_mut() {
            Some((_, sources)) => sources.append(&mut free),
            None if !free.is_empty() => groups.push((OnConflict::Ask, free)),
            None => {}
        }
        let last = groups.len().saturating_sub(1);
        let mut followup = followup;
        for (index, (policy, sources)) in groups.into_iter().enumerate() {
            let followup = if index == last { followup.take() } else { None };
            self.submit_transfer(&transfer, sources, policy, followup);
        }
    }

    fn submit_transfer(
        &mut self,
        transfer: &Transfer,
        sources: Vec<PathBuf>,
        on_conflict: OnConflict,
        followup: Option<Followup>,
    ) {
        let dest = transfer.dest.clone();
        let op = if transfer.copy {
            FileOp::Copy { sources, dest, on_conflict }
        } else {
            FileOp::Move { sources, dest, on_conflict }
        };
        self.submit(op, followup);
    }

    pub fn on_op(&mut self, event: OpEvent) {
        match event {
            OpEvent::Started { id, .. } => {
                if let Some(view) = self.operations.list.iter_mut().find(|v| v.id == id) {
                    view.running = true;
                }
            }
            OpEvent::Progress { id, progress } => {
                if let Some(view) = self.operations.list.iter_mut().find(|v| v.id == id) {
                    view.progress = progress;
                }
            }
            OpEvent::Finished { id, op, result } => {
                self.operations.list.retain(|view| view.id != id);
                let followup = self.operations.followups.remove(&id);
                let undoing = self.operations.undoing.remove(&id);
                match &result {
                    Ok(outcome @ OpOutcome::Done { created, .. }) => {
                        if !undoing && let Some(inverse) = op.inverse(outcome) {
                            self.operations.record(op.describe(), UndoEntry::Ops(inverse));
                        }
                        self.labels_follow(&op, outcome);
                        self.after_op(followup, created.first().cloned());
                    }
                    Ok(OpOutcome::Aborted) => self.set_status("операция отменена", Level::Info),
                    Err(error) => self.set_status(error.clone(), Level::Error),
                }
                self.reload_dirs(&op.affected_dirs());
                if matches!(op, FileOp::Delete { permanent: false, .. } | FileOp::Restore { .. }) {
                    self.workers.recycle_bin();
                }
            }
        }
    }

    /// Цветная метка переименованного или перемещённого объекта — за ним.
    fn labels_follow(&mut self, op: &FileOp, outcome: &OpOutcome) {
        let (FileOp::Rename { .. } | FileOp::Move { .. }, OpOutcome::Done { pairs, .. }) =
            (op, outcome)
        else {
            return;
        };
        let mut changed = false;
        for (from, to) in pairs {
            changed |= self.labels.moved(from, to);
        }
        if changed {
            self.save_labels();
        }
    }

    fn after_op(&mut self, followup: Option<Followup>, created: Option<PathBuf>) {
        match (followup, created) {
            (Some(Followup::RenameCreated { tab }), Some(path)) => {
                if let Some(tab) = self.tab_by_id(tab) {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    tab.pending_select = Some(path.clone());
                    tab.rename = Some(InlineRename { path, text: name, fresh: true });
                }
            }
            (Some(Followup::SelectCreated { tab }), Some(path)) => {
                if let Some(tab) = self.tab_by_id(tab) {
                    tab.pending_select = Some(path);
                }
            }
            (Some(Followup::ClearClipboard), _) => {
                self.cut.clear();
                self.workers.shell(ShellJob::ClearClipboard);
            }
            _ => {}
        }
    }

    /// Пакетное переименование: записать обратный план, если оно удалось.
    pub fn start_batch_rename(&mut self, plan: Plan) {
        self.operations.batch = Some(plan.clone());
        self.workers.batch_rename(Ticket { owner: OWNER_BATCH, generation: 0 }, plan);
    }

    pub fn on_batch_done(&mut self, ok: bool) {
        if let Some(plan) = self.operations.batch.take()
            && ok
        {
            let label = format!("Пакетное переименование: {}", plan.moves.len());
            let reverse =
                Plan { moves: plan.moves.into_iter().map(|(from, to)| (to, from)).collect() };
            self.operations.record(label, UndoEntry::Rename(reverse));
        }
    }

    /// Отменить последнее действие.
    pub fn undo(&mut self) {
        let Some((label, entry)) = self.operations.journal.pop() else {
            self.set_status("отменять нечего", Level::Info);
            return;
        };
        match entry {
            UndoEntry::Ops(ops) => {
                for op in ops {
                    let id = self.submit(op, None);
                    self.operations.undoing.insert(id);
                }
            }
            UndoEntry::Rename(plan) => {
                // Обратный план не записывается в журнал: batch остаётся пустым.
                self.workers.batch_rename(Ticket { owner: OWNER_BATCH, generation: 1 }, plan);
            }
            UndoEntry::Sort(journal) => {
                // Итог сообщит сам сортировщик, когда вернёт файлы.
                self.undo_sort(journal);
                return;
            }
        }
        self.set_status(format!("отменено: {label}"), Level::Info);
    }
}
