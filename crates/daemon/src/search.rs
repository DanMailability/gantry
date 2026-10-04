//! Conversation search: a Tantivy index of what you typed and Claude's replies in the indexed Claude folders, one
//! document per message, kept beside the profile's logs. `ConversationIndex` builds and updates it (one writer, on
//! one thread); `ConversationSearcher` searches it from any thread.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use anyhow::Context as _;
use claude_index::search::{Speaker as MessageSpeaker, read_messages};
use protocol::message::{SearchHit, Speaker};
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::{LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term, doc};

/// Bump when the schema or the extracted text changes: an index of another version is rebuilt.
const VERSION: u32 = 2;
/// Index writer memory budget, on one thread.
const WRITER_BUDGET: usize = 50_000_000;
/// Messages between commits while indexing.
const COMMIT_EVERY: usize = 5_000;
/// Longest snippet, in characters.
const SNIPPET_CHARS: usize = 160;
/// Most index terms the last word's prefix expands to.
const PREFIX_TERMS: usize = 64;
/// Documents read per requested session (one session can have many matching messages).
const DOCS_PER_HIT: usize = 20;
/// Most documents read for one search.
const MAX_DOCS: usize = 5_000;
/// Tokenizer of the `words` field: like `en_stem` without the stemming.
const WORDS_TOKENIZER: &str = "gantry_words";

/// Lowercased words, unstemmed: what a partly typed word is matched against ("termina" is a prefix of "terminal",
/// not of its stem "termin").
fn words_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default()).filter(RemoveLongFilter::limit(40)).filter(LowerCaser).build()
}

/// The query of the stemmed terms a search marks in its snippets.
type Marks = Box<dyn Query>;

/// What each commit stores beside the documents.
#[derive(Default, Serialize, Deserialize)]
struct Payload {
    version: u32,
    /// Transcript path → bytes indexed.
    offsets: BTreeMap<PathBuf, u64>,
}

#[derive(Clone, Copy)]
struct Fields {
    /// Transcript path (exact; for deleting a file's documents).
    path: Field,
    session: Field,
    claude_dir: Field,
    speaker: Field,
    /// The message, stemmed (exact words and phrases); stored for snippets.
    text: Field,
    /// The message, unstemmed and not stored (the last word's prefix).
    words: Field,
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("en_stem")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let fields = Fields {
        path: builder.add_text_field("path", STRING),
        session: builder.add_text_field("session", STRING | STORED),
        claude_dir: builder.add_text_field("claude_dir", STORED),
        speaker: builder.add_text_field("speaker", STORED),
        text: builder.add_text_field("text", text),
        words: builder.add_text_field(
            "words",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(WORDS_TOKENIZER)
                    .set_index_option(IndexRecordOption::WithFreqs),
            ),
        ),
    };
    (builder.build(), fields)
}

/// What an indexing pass does to one transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Index its messages from byte `from`; `reread` = it shrank, so its documents go first (`from` is 0).
    Read {
        /// Transcript.
        path: PathBuf,
        /// Claude config dir it is in.
        claude_dir: PathBuf,
        /// First byte to read.
        from: u64,
        /// Whether its old documents are deleted first.
        reread: bool,
    },
    /// The file is gone: delete its documents.
    Remove {
        /// Transcript.
        path: PathBuf,
    },
}

/// The conversation index, with the one writer allowed (use from one thread).
pub struct ConversationIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    /// Committed transcript offsets.
    offsets: BTreeMap<PathBuf, u64>,
    /// Transcripts already reported as unreadable (logged once).
    unreadable: HashSet<PathBuf>,
}

impl ConversationIndex {
    /// Open the index in `dir`, creating it if missing. An index that cannot be opened, or that another version
    /// wrote, is deleted and created again; failing that is an error.
    pub fn open(dir: &Path) -> anyhow::Result<ConversationIndex> {
        Self::open_versioned(dir, VERSION)
    }

    fn open_versioned(dir: &Path, version: u32) -> anyhow::Result<ConversationIndex> {
        match Self::open_existing(dir, version) {
            Ok(Some(index)) => return Ok(index),
            Ok(None) => {}
            Err(err) => tracing::warn!("conversation index unusable, rebuilding: {err:#}"),
        }
        if dir.exists() {
            std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let (schema, fields) = schema();
        let index = Index::create_in_dir(dir, schema)?;
        let mut created = Self::with(index, fields, BTreeMap::new())?;
        // Stamp the version at once, so an index that is never written to is not rebuilt on every start.
        let mut writer = created.writer()?;
        created.commit(&mut writer, &BTreeMap::new(), version)?;
        Ok(created)
    }

    /// `Ok(None)`: no index there yet, or one of another version.
    fn open_existing(dir: &Path, version: u32) -> anyhow::Result<Option<ConversationIndex>> {
        if !dir.join("meta.json").exists() {
            return Ok(None);
        }
        let index = Index::open_in_dir(dir)?;
        let payload: Payload = match index.load_metas()?.payload {
            Some(json) => serde_json::from_str(&json)?,
            None => Payload::default(),
        };
        if payload.version != version {
            return Ok(None);
        }
        let (_, fields) = schema();
        Ok(Some(Self::with(index, fields, payload.offsets)?))
    }

    fn with(index: Index, fields: Fields, offsets: BTreeMap<PathBuf, u64>) -> anyhow::Result<ConversationIndex> {
        index.tokenizers().register(WORDS_TOKENIZER, words_analyzer());
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        Ok(ConversationIndex { index, reader, fields, offsets, unreadable: HashSet::new() })
    }

    fn writer(&self) -> anyhow::Result<IndexWriter> {
        Ok(self.index.writer_with_num_threads(1, WRITER_BUDGET)?)
    }

    /// A handle for searching from other threads; it sees every later commit.
    pub fn searcher(&self) -> ConversationSearcher {
        ConversationSearcher { index: self.index.clone(), reader: self.reader.clone(), fields: self.fields }
    }

    /// What indexing `dirs` needs now: transcripts that are new, grew or shrank, and known transcripts whose file is
    /// gone (whether or not their dir is listed).
    pub fn pending(&self, dirs: &[PathBuf]) -> Vec<Change> {
        let mut changes = Vec::new();
        let mut seen = HashSet::new();
        for dir in dirs {
            for (path, _, len) in claude_index::index::transcripts(&dir.join("projects")) {
                let read = |from, reread| Change::Read { path: path.clone(), claude_dir: dir.clone(), from, reread };
                match self.offsets.get(&path).copied() {
                    Some(at) if at == len => {}
                    Some(at) if at < len => changes.push(read(at, false)),
                    Some(_) => changes.push(read(0, true)),
                    None => changes.push(read(0, false)),
                }
                seen.insert(path);
            }
        }
        for path in self.offsets.keys() {
            if !seen.contains(path) && !path.exists() {
                changes.push(Change::Remove { path: path.clone() });
            }
        }
        changes
    }

    /// Apply `changes`, committing every `COMMIT_EVERY` messages and at the end; `progress(done, total)` follows
    /// each file. On an error the uncommitted part is dropped with its offsets, so the next pass reads it again.
    pub fn apply(&mut self, changes: &[Change], progress: &mut dyn FnMut(u64, u64)) -> anyhow::Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer()?;
        let mut offsets = self.offsets.clone();
        let (mut since_commit, mut dirty) = (0usize, false);
        let total = changes.len() as u64;
        for (done, change) in changes.iter().enumerate() {
            match change {
                Change::Remove { path } => {
                    writer.delete_term(self.path_term(path));
                    offsets.remove(path);
                    dirty = true;
                }
                Change::Read { path, claude_dir, from, reread } => match read_messages(path, *from) {
                    Ok((messages, end)) => {
                        if *reread {
                            writer.delete_term(self.path_term(path));
                            dirty = true;
                        }
                        let session = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        for message in &messages {
                            writer.add_document(doc!(
                                self.fields.path => path.to_string_lossy().as_ref(),
                                self.fields.session => session.as_str(),
                                self.fields.claude_dir => claude_dir.to_string_lossy().as_ref(),
                                self.fields.speaker => match message.speaker {
                                    MessageSpeaker::You => "you",
                                    MessageSpeaker::Claude => "claude",
                                },
                                self.fields.text => message.text.as_str(),
                                self.fields.words => message.text.as_str(),
                            ))?;
                        }
                        since_commit += messages.len();
                        dirty |= !messages.is_empty() || offsets.get(path) != Some(&end);
                        offsets.insert(path.clone(), end);
                    }
                    Err(err) => {
                        if self.unreadable.insert(path.clone()) {
                            tracing::warn!("cannot read {} for search: {err}", path.display());
                        }
                    }
                },
            }
            if since_commit >= COMMIT_EVERY {
                self.commit(&mut writer, &offsets, VERSION)?;
                (since_commit, dirty) = (0, false);
            }
            progress(done as u64 + 1, total);
        }
        if dirty {
            self.commit(&mut writer, &offsets, VERSION)?;
        }
        writer.wait_merging_threads()?;
        Ok(())
    }

    fn commit(
        &mut self,
        writer: &mut IndexWriter,
        offsets: &BTreeMap<PathBuf, u64>,
        version: u32,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&Payload { version, offsets: offsets.clone() })?;
        let mut prepared = writer.prepare_commit()?;
        prepared.set_payload(&payload);
        prepared.commit()?;
        self.offsets = offsets.clone();
        self.reader.reload()?;
        Ok(())
    }

    fn path_term(&self, path: &Path) -> Term {
        Term::from_field_text(self.fields.path, &path.to_string_lossy())
    }

    #[cfg(test)]
    fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

/// Searches the conversation index (cheap to clone; any thread).
#[derive(Clone)]
pub struct ConversationSearcher {
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

/// Most characters kept before the first match: the pop-up shows one line, so a match late in the snippet would be
/// cut off.
const LEAD_CHARS: usize = 30;

/// `snippet` starting at most `LEAD_CHARS` characters before its first highlight (at a word start when there is one,
/// marked with "…"), with the highlights moved to match.
fn lead_to_match(snippet: String, highlights: Vec<(u32, u32)>) -> (String, Vec<(u32, u32)>) {
    let Some(&(first, _)) = highlights.first() else { return (snippet, highlights) };
    let before: Vec<(usize, char)> = snippet[..first as usize].char_indices().collect();
    if before.len() <= LEAD_CHARS {
        return (snippet, highlights);
    }
    let window = &before[before.len() - LEAD_CHARS..];
    // Start after a space inside the window when there is one, so no word is cut in half.
    let cut =
        window.iter().position(|&(_, c)| c == ' ').and_then(|i| window.get(i + 1)).map_or(window[0].0, |&(at, _)| at);
    let shifted = snippet[cut..].to_string();
    let lead = "…";
    let moved = |at: u32| at - cut as u32 + lead.len() as u32;
    (format!("{lead}{shifted}"), highlights.iter().map(|&(start, end)| (moved(start), moved(end))).collect())
}

/// One piece of a query.
#[derive(Debug, PartialEq)]
enum Part {
    Word(String),
    Phrase(String),
}

/// Split on whitespace, keeping closed `"…"` together; the words of an unclosed quote count as plain words.
fn split_query(query: &str) -> Vec<Part> {
    let words = |text: &str| text.split_whitespace().map(|w| Part::Word(w.into())).collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut rest = query;
    while let Some(start) = rest.find('"') {
        parts.extend(words(&rest[..start]));
        let after = &rest[start + 1..];
        match after.find('"') {
            Some(end) => {
                parts.push(Part::Phrase(after[..end].into()));
                rest = &after[end + 1..];
            }
            None => {
                rest = after;
                break;
            }
        }
    }
    parts.extend(words(rest));
    parts
}

impl ConversationSearcher {
    /// Sessions whose messages match `query`, best first: one hit per session (its best message), only sessions
    /// `known` accepts, at most `limit`. A query without words finds nothing.
    pub fn search(&self, query: &str, limit: usize, known: &dyn Fn(&str) -> bool) -> anyhow::Result<Vec<SearchHit>> {
        let searcher = self.reader.searcher();
        let Some((query, marked)) = self.build_query(&searcher, query)? else { return Ok(Vec::new()) };
        let wanted = (limit.saturating_mul(DOCS_PER_HIT)).clamp(1, MAX_DOCS);
        let top = searcher.search(&*query, &TopDocs::with_limit(wanted).order_by_score())?;
        let mut snippets = SnippetGenerator::create(&searcher, &*marked, self.fields.text)?;
        snippets.set_max_num_chars(SNIPPET_CHARS);
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for (_, address) in top {
            if hits.len() == limit {
                break;
            }
            let doc: TantivyDocument = searcher.doc(address)?;
            let field = |field| doc.get_first(field).and_then(|value| value.as_str()).unwrap_or_default().to_string();
            let session = field(self.fields.session);
            if !known(&session) || !seen.insert(session.clone()) {
                continue;
            }
            let found = snippets.snippet_from_doc(&doc);
            let (snippet, highlights) = if found.fragment().is_empty() {
                (field(self.fields.text).chars().take(SNIPPET_CHARS).collect::<String>(), Vec::new())
            } else {
                let ranges = found.highlighted().iter().map(|r| (r.start as u32, r.end as u32)).collect();
                (found.fragment().to_string(), ranges)
            };
            // One line; each replaced character is one byte, so the ranges still fit.
            let (snippet, highlights) = lead_to_match(snippet.replace(['\n', '\r', '\t'], " "), highlights);
            hits.push(SearchHit {
                session_id: session,
                claude_dir: PathBuf::from(field(self.fields.claude_dir)),
                speaker: if field(self.fields.speaker) == "claude" { Speaker::Claude } else { Speaker::You },
                snippet,
                highlights,
            });
        }
        Ok(hits)
    }

    /// The query, and the stemmed terms to mark in snippets. Every word must match (stemmed); a closed quote is a
    /// phrase; the last plain word also matches any word it begins (unstemmed, so "termina" finds "terminal").
    fn build_query(&self, searcher: &Searcher, query: &str) -> anyhow::Result<Option<(Box<dyn Query>, Marks)>> {
        let mut stemmer = self.index.tokenizer_for_field(self.fields.text)?;
        let mut plain = words_analyzer();
        let tokens = |analyzer: &mut TextAnalyzer, text: &str| {
            let mut out = Vec::new();
            let mut stream = analyzer.token_stream(text);
            while stream.advance() {
                out.push(stream.token().text.clone());
            }
            out
        };
        let term = |text: &str| Term::from_field_text(self.fields.text, text);
        let term_query =
            |term: Term| -> Box<dyn Query> { Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)) };
        let parts = split_query(query);
        let last_word = parts.iter().rposition(|part| matches!(part, Part::Word(_)));
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let mut marks: Vec<Term> = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            match part {
                Part::Phrase(text) => {
                    let terms: Vec<Term> = tokens(&mut stemmer, text).iter().map(|t| term(t)).collect();
                    marks.extend(terms.iter().cloned());
                    match terms.len() {
                        0 => {}
                        1 => clauses.push((Occur::Must, term_query(terms[0].clone()))),
                        _ => clauses.push((Occur::Must, Box::new(PhraseQuery::new(terms)))),
                    }
                }
                Part::Word(word) => {
                    // "terminal_view.rs" is several tokens; each must match.
                    let raw = tokens(&mut plain, word);
                    for (j, token) in raw.iter().enumerate() {
                        let Some(stem) = tokens(&mut stemmer, token).into_iter().next() else { continue };
                        marks.push(term(&stem));
                        let exact = term_query(term(&stem));
                        if Some(i) == last_word && j + 1 == raw.len() {
                            let mut either = vec![(Occur::Should, exact)];
                            for word in self.prefix_words(searcher, token)? {
                                either
                                    .push((Occur::Should, term_query(Term::from_field_text(self.fields.words, &word))));
                                // Snippets are marked on the stemmed field: mark the stem of each word found.
                                marks.extend(tokens(&mut stemmer, &word).first().map(|stem| term(stem)));
                            }
                            clauses.push((Occur::Must, Box::new(BooleanQuery::new(either))));
                        } else {
                            clauses.push((Occur::Must, exact));
                        }
                    }
                }
            }
        }
        if clauses.is_empty() {
            return Ok(None);
        }
        marks.dedup();
        let marked: Box<dyn Query> =
            Box::new(BooleanQuery::new(marks.into_iter().map(|t| (Occur::Should, term_query(t))).collect()));
        Ok(Some((Box::new(BooleanQuery::new(clauses)), marked)))
    }

    /// Up to `PREFIX_TERMS` indexed words (unstemmed) starting with `prefix`.
    fn prefix_words(&self, searcher: &Searcher, prefix: &str) -> anyhow::Result<Vec<String>> {
        // No UTF-8 byte is 0xff, so every term starting with `prefix` sorts below `prefix` + 0xff.
        let mut upper = prefix.as_bytes().to_vec();
        upper.push(0xff);
        let mut words: Vec<String> = Vec::new();
        for segment in searcher.segment_readers() {
            let inverted = segment.inverted_index(self.fields.words)?;
            let mut stream = inverted.terms().range().ge(prefix.as_bytes()).lt(&upper).into_stream()?;
            while words.len() < PREFIX_TERMS && stream.advance() {
                if let Ok(text) = std::str::from_utf8(stream.key())
                    && !words.iter().any(|word| word == text)
                {
                    words.push(text.to_string());
                }
            }
        }
        Ok(words)
    }
}

/// Time between indexing passes (the most a new message waits to become searchable).
const PASS_EVERY: Duration = Duration::from_secs(30);
/// A pass over at least this many files reports its progress.
const PROGRESS_FROM: usize = 20;
/// Least time between progress reports.
const PROGRESS_EVERY: Duration = Duration::from_secs(1);

/// What the search thread tells the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    /// A large pass is running: `done` of `total` files (once with `done == total` at its end).
    Indexing {
        /// Files done.
        done: u64,
        /// Files in the pass.
        total: u64,
    },
    /// Search stopped working for this daemon run.
    Unavailable {
        /// Why.
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Ready,
    Indexing { done: u64, total: u64 },
    Unavailable(String),
}

struct Inner {
    searcher: Option<ConversationSearcher>,
    status: Status,
}

/// The server's handle on conversation search (cheap to clone).
#[derive(Clone)]
pub struct Search {
    dirs: Sender<Vec<PathBuf>>,
    inner: Arc<Mutex<Inner>>,
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Search {
    /// Open the index in `dir` and keep it current on a thread of its own, at the lowest priority. Problems go to
    /// `events` (and the log); they never reach the panes.
    pub fn spawn(dir: PathBuf, events: UnboundedSender<SearchEvent>) -> Search {
        let (dirs, dirs_rx) = std::sync::mpsc::channel();
        let inner = Arc::new(Mutex::new(Inner { searcher: None, status: Status::Ready }));
        let thread_inner = inner.clone();
        let thread_events = events.clone();
        let spawned = std::thread::Builder::new().name("conversation-search".into()).spawn(move || {
            lower_priority();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&dir, &dirs_rx, &thread_inner, &thread_events)
            }));
            let reason = match outcome {
                Ok(Ok(())) => return,
                Ok(Err(err)) => format!("{err:#}"),
                Err(_) => "the indexer panicked".to_string(),
            };
            unavailable(&thread_inner, &thread_events, reason);
        });
        if let Err(err) = spawned {
            unavailable(&inner, &events, format!("could not start the indexer: {err}"));
        }
        Search { dirs, inner }
    }

    /// Index these Claude config dirs from now on.
    pub fn set_dirs(&self, dirs: Vec<PathBuf>) {
        let _ = self.dirs.send(dirs);
    }

    /// Hits for `query` (see `ConversationSearcher::search`); none while the index is still opening. `Err` with the
    /// reason once search is unavailable.
    pub fn search(&self, query: &str, limit: usize, known: &dyn Fn(&str) -> bool) -> Result<Vec<SearchHit>, String> {
        let (searcher, status) = {
            let inner = lock(&self.inner);
            (inner.searcher.clone(), inner.status.clone())
        };
        if let Status::Unavailable(reason) = status {
            return Err(reason);
        }
        let Some(searcher) = searcher else { return Ok(Vec::new()) };
        Ok(searcher.search(query, limit, known).unwrap_or_else(|err| {
            tracing::warn!("conversation search failed: {err:#}");
            Vec::new()
        }))
    }

    /// Progress of a large pass while it runs.
    pub fn indexing(&self) -> Option<(u64, u64)> {
        match lock(&self.inner).status {
            Status::Indexing { done, total } => Some((done, total)),
            _ => None,
        }
    }
}

fn unavailable(inner: &Mutex<Inner>, events: &UnboundedSender<SearchEvent>, reason: String) {
    tracing::error!("conversation search is off: {reason}");
    lock(inner).status = Status::Unavailable(reason.clone());
    let _ = events.send(SearchEvent::Unavailable { reason });
}

/// The search thread: open, then a pass over the current dirs every `PASS_EVERY` or when they change. Ends when
/// the `Search` handles are gone.
fn run(
    dir: &Path,
    dirs_rx: &Receiver<Vec<PathBuf>>,
    inner: &Mutex<Inner>,
    events: &UnboundedSender<SearchEvent>,
) -> anyhow::Result<()> {
    let mut index = ConversationIndex::open(dir)?;
    lock(inner).searcher = Some(index.searcher());
    let Ok(mut dirs) = dirs_rx.recv() else { return Ok(()) };
    loop {
        while let Ok(newer) = dirs_rx.try_recv() {
            dirs = newer;
        }
        let changes = index.pending(&dirs);
        let report = changes.len() >= PROGRESS_FROM;
        let total = changes.len() as u64;
        let mut last: Option<Instant> = None;
        let result = index.apply(&changes, &mut |done, total| {
            if report && done < total && last.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY) {
                last = Some(Instant::now());
                lock(inner).status = Status::Indexing { done, total };
                let _ = events.send(SearchEvent::Indexing { done, total });
            }
        });
        if report {
            lock(inner).status = Status::Ready;
            let _ = events.send(SearchEvent::Indexing { done: total, total });
        }
        if let Err(err) = result {
            tracing::warn!("conversation indexing failed, retrying next pass: {err:#}");
        }
        match dirs_rx.recv_timeout(PASS_EVERY) {
            Ok(newer) => dirs = newer,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// Run this thread (and the merge threads it starts) at the lowest priority: indexing must never slow the panes.
#[cfg(target_os = "linux")]
fn lower_priority() {
    // On Linux, `who = 0` with PRIO_PROCESS is the calling thread only.
    if let Err(err) = rustix::process::setpriority_process(None, 19) {
        tracing::debug!("could not lower the indexer's priority: {err}");
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_priority() {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn line(speaker: &str, text: &str) -> String {
        match speaker {
            "you" => json!({"type": "user", "cwd": "/r", "message": {"content": text}}).to_string(),
            _ => json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}}).to_string(),
        }
    }

    /// Write `projects/p/<id>.jsonl` under `claude` with `lines` (speaker, text).
    fn transcript(claude: &Path, id: &str, lines: &[(&str, &str)]) -> PathBuf {
        let dir = claude.join("projects").join("p");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        let text: String = lines.iter().map(|(s, t)| line(s, t) + "\n").collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn append(path: &Path, speaker: &str, text: &str) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{}", line(speaker, text)).unwrap();
    }

    struct Fixture {
        _root: tempfile::TempDir,
        claude: PathBuf,
        index_dir: PathBuf,
        index: ConversationIndex,
    }

    impl Fixture {
        fn new() -> Fixture {
            let root = tempfile::tempdir().unwrap();
            let claude = root.path().join("claude");
            let index_dir = root.path().join("search");
            let index = ConversationIndex::open(&index_dir).unwrap();
            Fixture { claude, index_dir, index, _root: root }
        }

        fn sync(&mut self) {
            let dirs = vec![self.claude.clone()];
            let changes = self.index.pending(&dirs);
            self.index.apply(&changes, &mut |_, _| {}).unwrap();
        }

        fn ids(&self, query: &str) -> Vec<String> {
            self.index.searcher().search(query, 50, &|_| true).unwrap().into_iter().map(|h| h.session_id).collect()
        }
    }

    #[test]
    fn word_forms_match() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "what about pricing for teams")]);
        f.sync();
        assert_eq!(f.ids("prices"), vec!["s1"]);
        assert_eq!(f.ids("PRICE"), vec!["s1"]);
    }

    #[test]
    fn every_word_must_match() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "font fallback on ubuntu")]);
        transcript(&f.claude, "s2", &[("you", "font sizes")]);
        f.sync();
        assert_eq!(f.ids("font ubuntu"), vec!["s1"]);
    }

    #[test]
    fn quoted_phrases_match_in_order() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "the font fallback works")]);
        transcript(&f.claude, "s2", &[("you", "fallback for the font")]);
        f.sync();
        assert_eq!(f.ids("\"font fallback\""), vec!["s1"]);
    }

    #[test]
    fn the_last_word_matches_as_a_prefix() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "the fallback on ubuntu")]);
        f.sync();
        assert_eq!(f.ids("fallb"), vec!["s1"]);
        assert_eq!(f.ids("ubuntu fallb"), vec!["s1"]);
        assert!(f.ids("fallb ubuntu").is_empty(), "only the last word is a prefix");
    }

    #[test]
    fn every_typed_prefix_of_a_word_finds_it() {
        // Stemming shortens these ("terminal" → "termin", "running" → "run"); a partly typed word must still match
        // while the user types it out.
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "the terminal configuration was running pricing")]);
        f.sync();
        for word in ["terminal", "configuration", "running", "pricing"] {
            for end in 3..=word.len() {
                assert_eq!(f.ids(&word[..end]), vec!["s1"], "typing {:?}", &word[..end]);
            }
        }
    }

    #[test]
    fn the_snippet_starts_close_to_the_match() {
        // The pop-up shows one line of about 90 characters: a match late in the fragment would be cut off.
        let mut f = Fixture::new();
        let long = "I looked through the repository layout first. The build uses a small script, and the deploy \
                    step copies the static pages; the mascot image is a quokka.png that the header loads.";
        transcript(&f.claude, "s1", &[("claude", long)]);
        f.sync();
        let hit = &f.index.searcher().search("mascot", 50, &|_| true).unwrap()[0];
        let (start, end) = hit.highlights[0];
        assert_eq!(&hit.snippet[start as usize..end as usize], "mascot");
        let before = hit.snippet[..start as usize].chars().count();
        assert!(before <= 35, "{before} characters before the match: {:?}", hit.snippet);
        assert!(hit.snippet.starts_with('…'), "{:?}", hit.snippet);
    }

    #[test]
    fn a_match_near_the_start_keeps_the_start() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "the mascot is a quokka")]);
        f.sync();
        let hit = &f.index.searcher().search("mascot", 50, &|_| true).unwrap()[0];
        assert_eq!(hit.snippet, "the mascot is a quokka");
    }

    #[test]
    fn a_partly_typed_word_is_marked_in_the_snippet() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("claude", "open the terminal now")]);
        f.sync();
        let hit = &f.index.searcher().search("termina", 50, &|_| true).unwrap()[0];
        let marked: Vec<&str> = hit.highlights.iter().map(|&(s, e)| &hit.snippet[s as usize..e as usize]).collect();
        assert_eq!(marked, vec!["terminal"]);
    }

    #[test]
    fn symbols_are_text() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("claude", "edited terminal_view.rs today")]);
        f.sync();
        assert_eq!(f.ids("terminal_view.rs"), vec!["s1"]);
        assert_eq!(f.ids("view:rs*"), vec!["s1"]);
    }

    #[test]
    fn odd_queries_do_not_fail() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "unclosed quotes and anything at all")]);
        f.sync();
        for query in ["", "   ", "\"\"", "::*", "\"unclosed any", "a", "-", "\"", "AND OR NOT"] {
            f.index.searcher().search(query, 50, &|_| true).unwrap();
        }
        assert_eq!(f.ids("\"unclosed any"), vec!["s1"], "an unclosed quote's words count as plain words");
    }

    #[test]
    fn one_hit_per_session_best_first() {
        let mut f = Fixture::new();
        transcript(
            &f.claude,
            "weak",
            &[("you", "a long message that mentions zebra once among many other words here")],
        );
        transcript(&f.claude, "strong", &[("you", "zebra zebra zebra")]);
        f.sync();
        assert_eq!(f.ids("zebra"), vec!["strong", "weak"]);
    }

    #[test]
    fn the_snippet_marks_the_match_and_names_the_speaker() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("claude", "DirectWrite does not list Figtree, so the fallback picks Noto")]);
        f.sync();
        let hits = f.index.searcher().search("fallback", 50, &|_| true).unwrap();
        let hit = &hits[0];
        assert_eq!(hit.speaker, Speaker::Claude);
        assert_eq!(hit.claude_dir, f.claude);
        let (start, end) = hit.highlights[0];
        assert_eq!(&hit.snippet[start as usize..end as usize], "fallback");
        assert!(hit.snippet.chars().count() <= 160);
    }

    #[test]
    fn snippets_are_one_line() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "first line\nsecond fallback line")]);
        f.sync();
        let hit = &f.index.searcher().search("fallback", 50, &|_| true).unwrap()[0];
        assert!(!hit.snippet.contains('\n'), "{:?}", hit.snippet);
        let (start, end) = hit.highlights[0];
        assert_eq!(&hit.snippet[start as usize..end as usize], "fallback");
    }

    #[test]
    fn non_ascii_snippets_keep_char_boundaries() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "שלום עולם fallback 🎉 עוד טקסט")]);
        f.sync();
        let hit = &f.index.searcher().search("fallback", 50, &|_| true).unwrap()[0];
        for (start, end) in &hit.highlights {
            assert!(hit.snippet.get(*start as usize..*end as usize).is_some());
        }
    }

    #[test]
    fn only_known_sessions_are_returned() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "kiwi")]);
        transcript(&f.claude, "s2", &[("you", "kiwi")]);
        f.sync();
        let hits = f.index.searcher().search("kiwi", 50, &|id| id == "s2").unwrap();
        assert_eq!(hits.iter().map(|h| h.session_id.as_str()).collect::<Vec<_>>(), vec!["s2"]);
    }

    #[test]
    fn the_limit_counts_sessions() {
        let mut f = Fixture::new();
        for i in 0..5 {
            transcript(&f.claude, &format!("s{i}"), &[("you", "mango"), ("claude", "mango again")]);
        }
        f.sync();
        assert_eq!(f.index.searcher().search("mango", 3, &|_| true).unwrap().len(), 3);
    }

    #[test]
    fn appended_lines_are_read_from_the_last_offset() {
        let mut f = Fixture::new();
        let path = transcript(&f.claude, "s1", &[("you", "first")]);
        f.sync();
        append(&path, "claude", "second papaya");
        let changes = f.index.pending(&[f.claude.clone()]);
        assert!(matches!(&changes[..], [Change::Read { from, reread: false, .. }] if *from > 0), "{changes:?}");
        f.sync();
        assert_eq!(f.ids("papaya"), vec!["s1"]);
        assert_eq!(f.index.num_docs(), 2);
        assert!(f.index.pending(&[f.claude.clone()]).is_empty());
    }

    #[test]
    fn a_shrunk_file_is_read_again() {
        let mut f = Fixture::new();
        let path = transcript(&f.claude, "s1", &[("you", "old words here"), ("you", "more old words")]);
        f.sync();
        std::fs::write(&path, line("you", "new") + "\n").unwrap();
        f.sync();
        assert!(f.ids("old").is_empty());
        assert_eq!(f.ids("new"), vec!["s1"]);
        assert_eq!(f.index.num_docs(), 1);
    }

    #[test]
    fn a_deleted_file_loses_its_documents() {
        let mut f = Fixture::new();
        let path = transcript(&f.claude, "s1", &[("you", "cherry")]);
        f.sync();
        std::fs::remove_file(&path).unwrap();
        f.sync();
        assert!(f.ids("cherry").is_empty());
        assert_eq!(f.index.num_docs(), 0);
    }

    #[test]
    fn a_session_in_two_dirs_is_one_hit() {
        let mut f = Fixture::new();
        let other = f.claude.with_file_name("claude-private");
        transcript(&f.claude, "same", &[("you", "lychee")]);
        transcript(&other, "same", &[("you", "lychee")]);
        let changes = f.index.pending(&[f.claude.clone(), other.clone()]);
        f.index.apply(&changes, &mut |_, _| {}).unwrap();
        assert_eq!(f.ids("lychee"), vec!["same"]);
    }

    #[test]
    fn subagent_transcripts_are_not_indexed() {
        let mut f = Fixture::new();
        let deep = f.claude.join("projects").join("p").join("s1").join("subagents");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("agent-a.jsonl"), line("you", "durian") + "\n").unwrap();
        f.sync();
        assert!(f.ids("durian").is_empty());
    }

    #[test]
    fn reopening_keeps_the_offsets() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "fig")]);
        f.sync();
        drop(std::mem::replace(&mut f.index, ConversationIndex::open(&f.index_dir.with_file_name("unused")).unwrap()));
        f.index = ConversationIndex::open(&f.index_dir).unwrap();
        assert!(f.index.pending(&[f.claude.clone()]).is_empty());
        assert_eq!(f.ids("fig"), vec!["s1"]);
    }

    #[test]
    fn another_version_is_rebuilt() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "grape")]);
        f.sync();
        drop(std::mem::replace(&mut f.index, ConversationIndex::open(&f.index_dir.with_file_name("unused")).unwrap()));
        f.index = ConversationIndex::open_versioned(&f.index_dir, VERSION + 1).unwrap();
        assert_eq!(f.index.num_docs(), 0);
        assert_eq!(f.index.pending(&[f.claude.clone()]).len(), 1);
    }

    #[test]
    fn a_broken_index_is_rebuilt() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("search");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), "{ not json").unwrap();
        let index = ConversationIndex::open(&dir).unwrap();
        assert_eq!(index.num_docs(), 0);
    }

    #[test]
    fn progress_counts_files() {
        let mut f = Fixture::new();
        transcript(&f.claude, "s1", &[("you", "a")]);
        transcript(&f.claude, "s2", &[("you", "b")]);
        let changes = f.index.pending(&[f.claude.clone()]);
        let mut seen = Vec::new();
        f.index.apply(&changes, &mut |done, total| seen.push((done, total))).unwrap();
        assert_eq!(seen, vec![(1, 2), (2, 2)]);
    }

    #[test]
    fn searching_before_the_first_pass_finds_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let search = Search::spawn(root.path().join("search"), events);
        // No folders sent yet: the thread waits, and searching answers at once.
        assert_eq!(search.search("anything", 10, &|_| true), Ok(Vec::new()));
    }

    #[test]
    fn the_thread_indexes_the_dirs_it_is_given() {
        let root = tempfile::tempdir().unwrap();
        let claude = root.path().join("claude");
        transcript(&claude, "s1", &[("you", "persimmon")]);
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let search = Search::spawn(root.path().join("search"), events);
        search.set_dirs(vec![claude]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let hits = search.search("persimmon", 10, &|_| true).unwrap();
            if !hits.is_empty() {
                assert_eq!(hits[0].session_id, "s1");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "not indexed in time");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    fn an_index_that_cannot_open_makes_search_unavailable() {
        let root = tempfile::tempdir().unwrap();
        // A file where the index folder should be: neither opening nor creating can work.
        let blocked = root.path().join("search");
        std::fs::write(&blocked, "not a folder").unwrap();
        let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let search = Search::spawn(blocked.join("inner"), events);
        let event = rx.blocking_recv().expect("an event");
        assert!(matches!(event, SearchEvent::Unavailable { .. }), "{event:?}");
        assert!(search.search("x", 10, &|_| true).is_err());
    }
}
