use std::{
    collections::HashSet,
    iter,
    path::{Path, PathBuf},
    sync::Arc,
};

use dashmap::DashMap;
use futures_util::FutureExt;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{sinks, BinaryDetection, SearcherBuilder};
use helix_core::{
    syntax::{Loader, QueryMatchIterEvent},
    Rope, RopeSlice, Selection, Syntax, Uri,
};
use helix_stdx::{
    path,
    rope::{self, RopeSliceExt},
};
use helix_view::{
    align_view,
    document::{from_reader, SCRATCH_BUFFER_NAME},
    Align, Document, DocumentId, Editor,
};
use ignore::{DirEntry, WalkBuilder, WalkState};

use crate::{
    filter_picker_entry,
    ui::{
        overlay::overlaid,
        picker::{Injector, PathOrId},
        Picker, PickerColumn,
    },
};

use super::Context;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagRole {
    Definition,
    Reference,
}

impl TagRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Reference => "reference",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    // definition kinds
    Class,
    Constant,
    Enum,
    Field,
    Function,
    Interface,
    Macro,
    Module,
    Section,
    Struct,
    Type,
    // reference kinds
    Call,
    ClassRef,
    ImplementationRef,
}

impl TagKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Class | Self::ClassRef => "class",
            Self::Constant => "constant",
            Self::Enum => "enum",
            Self::Field => "field",
            Self::Function => "function",
            Self::Interface => "interface",
            Self::Macro => "macro",
            Self::Module => "module",
            Self::Section => "section",
            Self::Struct => "struct",
            Self::Type => "type",
            Self::Call => "call",
            Self::ImplementationRef => "implementation",
        }
    }

    fn from_capture_name(name: &str) -> Option<(TagRole, Self)> {
        let (role, kind) = name.split_once('.')?;
        match role {
            "definition" => match kind {
                "class" => Some((TagRole::Definition, Self::Class)),
                "constant" => Some((TagRole::Definition, Self::Constant)),
                "enum" => Some((TagRole::Definition, Self::Enum)),
                "field" => Some((TagRole::Definition, Self::Field)),
                "function" => Some((TagRole::Definition, Self::Function)),
                "interface" => Some((TagRole::Definition, Self::Interface)),
                "macro" => Some((TagRole::Definition, Self::Macro)),
                "module" => Some((TagRole::Definition, Self::Module)),
                "section" => Some((TagRole::Definition, Self::Section)),
                "struct" => Some((TagRole::Definition, Self::Struct)),
                "type" => Some((TagRole::Definition, Self::Type)),
                _ => None,
            },
            "reference" => match kind {
                "call" => Some((TagRole::Reference, Self::Call)),
                "class" => Some((TagRole::Reference, Self::ClassRef)),
                "implementation" => Some((TagRole::Reference, Self::ImplementationRef)),
                _ => None,
            },
            _ => None,
        }
    }
}

// NOTE: Uri is cheap to clone and DocumentId is Copy
#[derive(Debug, Clone)]
enum UriOrDocumentId {
    Uri(Uri),
    Id(DocumentId),
}

impl UriOrDocumentId {
    fn path_or_id(&self) -> Option<PathOrId<'_>> {
        match self {
            Self::Id(id) => Some(PathOrId::Id(*id)),
            Self::Uri(uri) => uri.as_path().map(PathOrId::Path),
        }
    }
}

#[derive(Debug)]
struct Tag {
    kind: TagKind,
    role: TagRole,
    name: String,
    /// Char offset of the `@name` capture start — used for cursor placement.
    start: usize,
    /// Char offset of the `@name` capture end — used for cursor placement.
    end: usize,
    /// Line of the tag node start — used for preview.
    start_line: usize,
    /// Line of the tag node end — used for preview.
    end_line: usize,
    doc: UriOrDocumentId,
}

fn tags_iter<'a>(
    syntax: &'a Syntax,
    loader: &'a Loader,
    text: RopeSlice<'a>,
    doc: UriOrDocumentId,
) -> impl Iterator<Item = Tag> + 'a {
    let mut tags_iter = syntax.tags(text, loader, ..);

    iter::from_fn(move || loop {
        let QueryMatchIterEvent::Match(mat) = tags_iter.next()? else {
            continue;
        };
        let query = &loader
            .tag_query(tags_iter.current_language())
            .expect("must have a tags query to emit matches")
            .query;

        // Find the @definition.*/@reference.* and optional @name captures in this match.
        let mut tag_capture = None::<(TagRole, TagKind, std::ops::Range<u32>)>;
        let mut name_range = None::<std::ops::Range<u32>>;
        let name_capture = query.get_capture("name");

        for node in mat.nodes.iter() {
            let capture_name = query.capture_name(node.capture);
            if let Some((role, kind)) = TagKind::from_capture_name(capture_name) {
                tag_capture = Some((role, kind, node.node.byte_range()));
            } else if name_capture == Some(node.capture) {
                name_range = Some(node.node.byte_range());
            }
        }

        let Some((role, kind, tag_byte_range)) = tag_capture else {
            continue;
        };
        let name_byte_range = name_range.unwrap_or_else(|| tag_byte_range.clone());

        let name_start = text.byte_to_char(name_byte_range.start as usize);
        let name_end = text.byte_to_char(name_byte_range.end as usize);
        let tag_start = text.byte_to_char(tag_byte_range.start as usize);
        let tag_end = text.byte_to_char(tag_byte_range.end as usize);

        return Some(Tag {
            kind,
            role,
            name: text.slice(name_start..name_end).to_string(),
            start: name_start,
            end: name_end,
            start_line: text.char_to_line(tag_start),
            end_line: text.char_to_line(tag_end),
            doc: doc.clone(),
        });
    })
}

/// Returns the `@name` text of the tag match at `cursor_byte`, or `None` if the
/// cursor is not on a tagged construct.
///
/// Scopes the query to the direct named child of the layer root that contains
/// the cursor, so that tag patterns whose anchor node (e.g. `function_item`)
/// starts before the cursor are still found.
fn find_name_at_cursor(
    syntax: &Syntax,
    loader: &Loader,
    text: RopeSlice,
    cursor_byte: u32,
) -> Option<String> {
    let tree = syntax.tree_for_byte_range(cursor_byte, cursor_byte);
    let root = tree.root_node();
    // Walk up from the deepest named node at cursor to find the direct child of root.
    let query_range = {
        let mut node = root
            .named_descendant_for_byte_range(cursor_byte, cursor_byte)
            .unwrap_or(root);
        loop {
            match node.parent() {
                None => break,
                Some(p) if p.parent().is_none() => break,
                Some(p) => node = p,
            }
        }
        node.start_byte()..node.end_byte()
    };

    let mut iter = syntax.tags(text, loader, query_range);
    while let Some(event) = iter.next() {
        let QueryMatchIterEvent::Match(mat) = event else { continue };
        let Some(tag_query) = loader.tag_query(iter.current_language()) else { continue };
        let query = &tag_query.query;
        let name_capture = query.get_capture("name");

        // Find the @name node for this match (may be absent in old-style patterns).
        let name_node = name_capture.and_then(|cap| {
            mat.nodes.iter().find(|n| n.capture == cap)
        });

        let has_tag = mat.nodes.iter().any(|n| {
            TagKind::from_capture_name(query.capture_name(n.capture)).is_some()
        });
        if !has_tag {
            continue;
        }

        if let Some(nn) = name_node {
            // New-style: cursor must be on the @name node.
            if nn.node.start_byte() <= cursor_byte && cursor_byte < nn.node.end_byte() {
                let start = text.byte_to_char(nn.node.start_byte() as usize);
                let end = text.byte_to_char(nn.node.end_byte() as usize);
                return Some(text.slice(start..end).to_string());
            }
        } else {
            // Old-style: no @name; cursor must be on the tag capture itself.
            for n in mat.nodes.iter() {
                if TagKind::from_capture_name(query.capture_name(n.capture)).is_some()
                    && n.node.start_byte() <= cursor_byte
                    && cursor_byte < n.node.end_byte()
                {
                    let start = text.byte_to_char(n.node.start_byte() as usize);
                    let end = text.byte_to_char(n.node.end_byte() as usize);
                    return Some(text.slice(start..end).to_string());
                }
            }
        }
    }
    None
}

pub fn syntax_symbol_picker(cx: &mut Context) {
    let doc = doc!(cx.editor);
    let Some(syntax) = doc.syntax() else {
        cx.editor
            .set_error("Syntax tree is not available on this buffer");
        return;
    };
    let doc_id = doc.id();
    let text = doc.text().slice(..);
    let loader = cx.editor.syn_loader.load();
    let tags = tags_iter(syntax, &loader, text, UriOrDocumentId::Id(doc.id()))
        .filter(|t| t.role == TagRole::Definition);

    let columns = vec![
        PickerColumn::new("kind", |tag: &Tag, _| tag.kind.as_str().into()),
        PickerColumn::new("name", |tag: &Tag, _| tag.name.as_str().into()),
    ];

    let picker = Picker::new(
        columns,
        1, // name
        tags,
        (),
        move |cx, tag, action| {
            cx.editor.switch(doc_id, action);
            let view = view_mut!(cx.editor);
            let doc = doc_mut!(cx.editor, &doc_id);
            doc.set_selection(view.id, Selection::single(tag.start, tag.end));
            if action.align_view(view, doc.id()) {
                align_view(doc, view, Align::Center)
            }
        },
    )
    .with_preview(|_editor, tag| {
        Some((tag.doc.path_or_id()?, Some((tag.start_line, tag.end_line))))
    })
    .truncate_start(false);

    cx.push_layer(Box::new(overlaid(picker)));
}

#[derive(Debug)]
struct WorkspaceSearchState {
    searcher_builder: SearcherBuilder,
    walk_builder: WalkBuilder,
    regex_matcher_builder: RegexMatcherBuilder,
    rope_regex_builder: rope::RegexBuilder,
    search_root: PathBuf,
    /// A cache of files that have been parsed in prior searches.
    syntax_cache: DashMap<PathBuf, Option<(Rope, Syntax)>>,
}

pub fn syntax_workspace_symbol_picker(cx: &mut Context) {
    type SearchState = WorkspaceSearchState;

    let mut searcher_builder = SearcherBuilder::new();
    searcher_builder.binary_detection(BinaryDetection::quit(b'\x00'));

    // Search from the workspace that the currently focused document is within. This behaves like global
    // search most of the time but helps when you have two projects open in splits.
    let search_root = if let Some(path) = doc!(cx.editor).path() {
        helix_loader::find_workspace_in(path).0
    } else {
        helix_loader::find_workspace().0
    };

    let absolute_root = search_root
        .canonicalize()
        .unwrap_or_else(|_| search_root.clone());

    let config = cx.editor.config();
    let dedup_symlinks = config.file_picker.deduplicate_links;

    let mut walk_builder = WalkBuilder::new(&search_root);
    walk_builder
        .hidden(config.file_picker.hidden)
        .parents(config.file_picker.parents)
        .ignore(config.file_picker.ignore)
        .follow_links(config.file_picker.follow_symlinks)
        .git_ignore(config.file_picker.git_ignore)
        .git_global(config.file_picker.git_global)
        .git_exclude(config.file_picker.git_exclude)
        .max_depth(config.file_picker.max_depth)
        .filter_entry(move |entry| filter_picker_entry(entry, &absolute_root, dedup_symlinks))
        .add_custom_ignore_filename(helix_loader::config_dir().join("ignore"))
        .add_custom_ignore_filename(".helix/ignore");

    let mut regex_matcher_builder = RegexMatcherBuilder::new();
    regex_matcher_builder.case_smart(config.search.smart_case);
    let mut rope_regex_builder = rope::RegexBuilder::new();
    rope_regex_builder.syntax(rope::Config::new().case_insensitive(config.search.smart_case));
    let state = SearchState {
        searcher_builder,
        walk_builder,
        regex_matcher_builder,
        rope_regex_builder,
        search_root,
        syntax_cache: DashMap::default(),
    };
    let reg = cx.register.unwrap_or('/');
    cx.editor.registers.last_search_register = reg;
    let columns = vec![
        PickerColumn::new("kind", |tag: &Tag, _| tag.kind.as_str().into()),
        PickerColumn::new("name", |tag: &Tag, _| tag.name.as_str().into()).without_filtering(),
        PickerColumn::new("path", |tag: &Tag, state: &SearchState| {
            match &tag.doc {
                UriOrDocumentId::Uri(uri) => {
                    if let Some(path) = uri.as_path() {
                        let path = if let Ok(stripped) = path.strip_prefix(&state.search_root) {
                            stripped
                        } else {
                            path
                        };
                        path.to_string_lossy().into()
                    } else {
                        uri.to_string().into()
                    }
                }
                // This picker only uses `Id` for scratch buffers for better display.
                UriOrDocumentId::Id(_) => SCRATCH_BUFFER_NAME.into(),
            }
        }),
    ];

    let get_tags = |query: &str,
                    editor: &mut Editor,
                    state: Arc<SearchState>,
                    injector: &Injector<_, _>| {
        if query.len() < 3 {
            return async { Ok(()) }.boxed();
        }
        // Attempt to find the tag in any open documents.
        let pattern = match state.rope_regex_builder.build(query) {
            Ok(pattern) => pattern,
            Err(err) => return async { Err(anyhow::anyhow!(err)) }.boxed(),
        };
        let loader = editor.syn_loader.load();
        for doc in editor.documents() {
            let Some(syntax) = doc.syntax() else { continue };
            let text = doc.text().slice(..);
            let uri_or_id = doc
                .uri()
                .map(UriOrDocumentId::Uri)
                .unwrap_or_else(|| UriOrDocumentId::Id(doc.id()));
            let text = text.slice(..);
            for tag in tags_iter(syntax, &loader, text, uri_or_id)
                .filter(|t| t.role == TagRole::Definition && pattern.is_match(
                    text.regex_input_at_bytes(text.char_to_byte(t.start)..text.char_to_byte(t.end)),
                ))
            {
                if injector.push(tag).is_err() {
                    return async { Ok(()) }.boxed();
                }
            }
        }
        if !state.search_root.exists() {
            return async { Err(anyhow::anyhow!("Current working directory does not exist")) }
                .boxed();
        }
        let matcher = match state.regex_matcher_builder.build(query) {
            Ok(matcher) => {
                // Clear any "Failed to compile regex" errors out of the statusline.
                editor.clear_status();
                matcher
            }
            Err(err) => {
                log::info!(
                    "Failed to compile search pattern in workspace symbol search: {}",
                    err
                );
                return async { Err(anyhow::anyhow!("Failed to compile regex")) }.boxed();
            }
        };
        let pattern = Arc::new(pattern);
        let injector = injector.clone();
        let loader = editor.syn_loader.load();

        let documents: HashSet<_> = editor
            .documents()
            .filter_map(Document::path)
            .map(ToOwned::to_owned)
            .collect();

        async move {
            let searcher = state.searcher_builder.build();
            state.walk_builder.build_parallel().run(|| {
                let mut searcher = searcher.clone();
                let matcher = matcher.clone();
                let injector = injector.clone();
                let loader = loader.clone();
                let documents = &documents;
                let pattern = pattern.clone();
                let syntax_cache = &state.syntax_cache;
                Box::new(move |entry: Result<DirEntry, ignore::Error>| -> WalkState {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => return WalkState::Continue,
                    };
                    if !entry.path().is_file() {
                        return WalkState::Continue;
                    }
                    let path = entry.path();
                    // If this document is open, skip it because we've already processed it above.
                    if documents.contains(path) {
                        return WalkState::Continue;
                    };
                    let mut quit = false;
                    let sink = sinks::UTF8(|_line, _content| {
                        if !syntax_cache.contains_key(path) {
                            // Read the file into a Rope and attempt to recognize the language
                            // and parse it with tree-sitter. Save the Rope and Syntax for future
                            // queries.
                            syntax_cache.insert(path.to_path_buf(), syntax_for_path(path, &loader));
                        };
                        let entry = syntax_cache.get(path).unwrap();
                        let Some((text, syntax)) = entry.value() else {
                            // If the file couldn't be parsed, move on.
                            return Ok(false);
                        };
                        let uri = Uri::from(path::normalize(path));
                        let text_slice = text.slice(..);
                        for tag in tags_iter(syntax, &loader, text_slice, UriOrDocumentId::Uri(uri))
                            .filter(|t| t.role == TagRole::Definition && pattern.is_match(
                                text_slice.regex_input_at_bytes(
                                    text_slice.char_to_byte(t.start)..text_slice.char_to_byte(t.end),
                                ),
                            ))
                        {
                            if injector.push(tag).is_err() {
                                quit = true;
                                break;
                            }
                        }
                        // Quit after seeing the first regex match. We only care to find files
                        // that contain the pattern and then we run the tags query within
                        // those. The location and contents of a match are irrelevant - it's
                        // only important _if_ a file matches.
                        Ok(false)
                    });
                    if let Err(err) = searcher.search_path(&matcher, path, sink) {
                        log::info!("Workspace syntax search error: {}, {}", path.display(), err);
                    }
                    if quit {
                        WalkState::Quit
                    } else {
                        WalkState::Continue
                    }
                })
            });
            Ok(())
        }
        .boxed()
    };
    let picker = Picker::new(
        columns,
        1, // name
        [],
        state,
        move |cx, tag, action| {
            let doc_id = match &tag.doc {
                UriOrDocumentId::Id(id) => *id,
                UriOrDocumentId::Uri(uri) => match cx.editor.open(uri.as_path().expect(""), action) {
                    Ok(id) => id,
                    Err(e) => {
                        cx.editor
                            .set_error(format!("Failed to open file '{uri:?}': {e}"));
                        return;
                    }
                }
            };
            let doc = doc_mut!(cx.editor, &doc_id);
            let view = view_mut!(cx.editor);
            let len_chars = doc.text().len_chars();
            if tag.start >= len_chars || tag.end > len_chars {
                cx.editor.set_error("The location you jumped to does not exist anymore because the file has changed.");
                return;
            }
            doc.set_selection(view.id, Selection::single(tag.start, tag.end));
            if action.align_view(view, doc.id()) {
                align_view(doc, view, Align::Center)
            }
        },
    )
    .with_dynamic_query(get_tags, Some(275))
    .with_preview(move |_editor, tag| {
        Some((
            tag.doc.path_or_id()?,
            Some((tag.start_line, tag.end_line)),
        ))
    })
    .with_history_register(Some(reg))
    .truncate_start(false);
    cx.push_layer(Box::new(overlaid(picker)));
}

/// Create a Rope and language config for a given existing path without creating a full Document.
fn syntax_for_path(path: &Path, loader: &Loader) -> Option<(Rope, Syntax)> {
    let mut file = std::fs::File::open(path).ok()?;
    let (rope, _encoding, _has_bom) = from_reader(&mut file, None).ok()?;
    let text = rope.slice(..);
    let language = loader
        .language_for_filename(path)
        .or_else(|| loader.language_for_shebang(text))?;
    Syntax::new(text, language, loader)
        .ok()
        .map(|syntax| (rope, syntax))
}


fn build_goto_walk_builder(cx: &mut Context, search_root: &std::path::Path) -> WalkBuilder {
    let config = cx.editor.config();
    let dedup_symlinks = config.file_picker.deduplicate_links;
    let absolute_root = search_root
        .canonicalize()
        .unwrap_or_else(|_| search_root.to_path_buf());
    let mut walk_builder = WalkBuilder::new(search_root);
    walk_builder
        .hidden(config.file_picker.hidden)
        .parents(config.file_picker.parents)
        .ignore(config.file_picker.ignore)
        .follow_links(config.file_picker.follow_symlinks)
        .git_ignore(config.file_picker.git_ignore)
        .git_global(config.file_picker.git_global)
        .git_exclude(config.file_picker.git_exclude)
        .max_depth(config.file_picker.max_depth)
        .filter_entry(move |entry| filter_picker_entry(entry, &absolute_root, dedup_symlinks))
        .add_custom_ignore_filename(helix_loader::config_dir().join("ignore"))
        .add_custom_ignore_filename(".helix/ignore");
    walk_builder
}

/// Spawn a background thread that walks the workspace, grepping for `name`, parsing
/// candidate files, running the tags query, and injecting matching `role` tags.
fn spawn_workspace_tag_scan(
    walk_builder: WalkBuilder,
    name: String,
    role: TagRole,
    loader: Arc<helix_core::syntax::Loader>,
    open_docs: Arc<HashSet<PathBuf>>,
    injector: Injector<Tag, PathBuf>,
) {
    let mut searcher_builder = SearcherBuilder::new();
    searcher_builder.binary_detection(BinaryDetection::quit(b'\x00'));
    // Use the name as a literal grep pattern to pre-filter files.
    let matcher = match RegexMatcherBuilder::new().build(&name) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("goto tag: failed to build grep matcher for {name:?}: {e}");
            return;
        }
    };
    std::thread::spawn(move || {
        let searcher = searcher_builder.build();
        walk_builder.build_parallel().run(|| {
            let mut searcher = searcher.clone();
            let matcher = matcher.clone();
            let injector = injector.clone();
            let loader = loader.clone();
            let name = name.clone();
            let open_docs = Arc::clone(&open_docs);
            Box::new(move |entry: Result<DirEntry, ignore::Error>| -> WalkState {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => return WalkState::Continue,
                };
                if !entry.path().is_file() {
                    return WalkState::Continue;
                }
                let path = entry.path();
                if open_docs.contains(path) {
                    return WalkState::Continue;
                }
                let mut quit = false;
                let mut processed = false;
                let sink = sinks::UTF8(|_, _| {
                    if !processed {
                        processed = true;
                        if let Some((rope, syntax)) = syntax_for_path(path, &loader) {
                            let uri = Uri::from(path::normalize(path));
                            for tag in
                                tags_iter(&syntax, &loader, rope.slice(..), UriOrDocumentId::Uri(uri))
                                    .filter(|t| t.role == role && t.name == name)
                            {
                                if injector.push(tag).is_err() {
                                    quit = true;
                                    break;
                                }
                            }
                        }
                    }
                    Ok(false)
                });
                if let Err(e) = searcher.search_path(&matcher, path, sink) {
                    log::info!("goto tag search error: {}, {e}", path.display());
                }
                if quit {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });
    });
}

pub fn syntax_goto_definition(cx: &mut Context) {
    syntax_goto_tags(cx, TagRole::Definition, "definition");
}

pub fn syntax_goto_references(cx: &mut Context) {
    syntax_goto_tags(cx, TagRole::Reference, "references");
}

fn syntax_goto_tags(cx: &mut Context, role: TagRole, role_label: &str) {
    let (view, doc) = current_ref!(cx.editor);
    let text = doc.text().slice(..);
    let cursor = doc.selection(view.id).primary().cursor(text);
    let cursor_byte = text.char_to_byte(cursor) as u32;
    let doc_id = doc.id();
    let loader_guard = cx.editor.syn_loader.load();
    let loader = Arc::clone(&*loader_guard);

    let Some(syntax) = doc.syntax() else {
        cx.editor
            .set_error("Syntax tree is not available on this buffer");
        return;
    };

    let Some(cursor_name) = find_name_at_cursor(syntax, &*loader, text, cursor_byte) else {
        cx.editor.set_error("No tag at cursor");
        return;
    };

    let search_root = if let Some(path) = doc.path() {
        helix_loader::find_workspace_in(path).0
    } else {
        helix_loader::find_workspace().0
    };

    let doc_uri_or_id = doc
        .uri()
        .map(UriOrDocumentId::Uri)
        .unwrap_or(UriOrDocumentId::Id(doc_id));

    // Collect in-file results now, while `syntax` and `text` are still borrowed.
    let in_file_tags: Vec<Tag> = tags_iter(syntax, &*loader, text, doc_uri_or_id)
        .filter(|t| t.role == role && t.name == cursor_name)
        .collect();
    // Also capture open-doc paths before releasing the borrow.
    let open_docs: Arc<HashSet<PathBuf>> = Arc::new(
        cx.editor
            .documents()
            .filter_map(Document::path)
            .map(ToOwned::to_owned)
            .collect(),
    );
    // `syntax`, `text`, `view`, `doc` borrows end here; `cx` can be borrowed mutably again.

    let walk_builder = build_goto_walk_builder(cx, &search_root);

    let columns = vec![
        PickerColumn::new("kind", |tag: &Tag, _| {
            format!("{}.{}", tag.role.as_str(), tag.kind.as_str()).into()
        }),
        PickerColumn::new("path", |tag: &Tag, search_root: &PathBuf| {
            match &tag.doc {
                UriOrDocumentId::Uri(uri) => {
                    if let Some(p) = uri.as_path() {
                        let rel = p.strip_prefix(search_root).unwrap_or(p);
                        format!("{}:{}", rel.display(), tag.start_line + 1).into()
                    } else {
                        uri.to_string().into()
                    }
                }
                UriOrDocumentId::Id(_) => SCRATCH_BUFFER_NAME.into(),
            }
        }),
    ];

    let picker = Picker::new(
        columns,
        1,
        [],
        search_root.clone(),
        |cx, tag: &Tag, action| {
            let doc_id = match &tag.doc {
                UriOrDocumentId::Id(id) => *id,
                UriOrDocumentId::Uri(uri) => {
                    match cx.editor.open(uri.as_path().expect("tag URI must be a path"), action) {
                        Ok(id) => id,
                        Err(e) => {
                            cx.editor
                                .set_error(format!("Failed to open file '{uri:?}': {e}"));
                            return;
                        }
                    }
                }
            };
            let doc = doc_mut!(cx.editor, &doc_id);
            let view = view_mut!(cx.editor);
            let len_chars = doc.text().len_chars();
            if tag.start >= len_chars || tag.end > len_chars {
                cx.editor.set_error(
                    "The location you jumped to no longer exists because the file has changed.",
                );
                return;
            }
            doc.set_selection(view.id, Selection::single(tag.start, tag.end));
            if action.align_view(view, doc.id()) {
                align_view(doc, view, Align::Center)
            }
        },
    )
    .with_preview(|_, tag| Some((tag.doc.path_or_id()?, Some((tag.start_line, tag.end_line)))))
    .truncate_start(false);

    let injector = picker.injector();

    // Inject the already-collected in-file results.
    for tag in in_file_tags {
        injector.push(tag).ok();
    }

    // Async: walk the workspace in a background thread.
    if search_root.exists() {
        spawn_workspace_tag_scan(
            walk_builder,
            cursor_name.clone(),
            role,
            loader,
            open_docs,
            injector,
        );
    }

    cx.editor.set_status(format!(
        "Searching for {role_label} of '{cursor_name}'...",
    ));
    cx.push_layer(Box::new(overlaid(picker)));
}
