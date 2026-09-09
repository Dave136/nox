use crate::app::{AppState, Nox};
use crate::nav::ActiveView;
use crate::theme::{APP_FONT_FAMILY, Theme};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, Entity, Focusable, FontWeight,
    PathPromptOptions, SharedString, Window, div, ease_out_quint, prelude::*, px, rgb,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, Sizable, WindowExt,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputState, Textarea, TextareaState},
    popover::Popover,
    radio::{Radio, RadioGroup},
    switch::Switch,
};
use gpui_rsx::rsx;
use nox_core::{
    CharClasses, ITEM_SCHEMA_VERSION, IconChoice, ItemId, ItemPayload, ItemType, MAX_LENGTH,
    NoteColor, Password, Vault, VaultError, generate_password, normalize_note_tags,
};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Identifies one *opening* of the editor, which `EditorMode` cannot: two
/// successive `Create` editors are equal as modes but are different editors,
/// and a favicon fetch started in the first must not land in the second.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EditorId(u64);

static NEXT_EDITOR_ID: AtomicU64 = AtomicU64::new(0);
const NOTE_PREVIEW_TRANSITION_DURATION: Duration = Duration::from_millis(190);

impl EditorId {
    fn next() -> Self {
        Self(NEXT_EDITOR_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditorMode {
    Create,
    Edit(ItemId),
}

fn sheet_title(mode: EditorMode, item_type: ItemType) -> &'static str {
    match (mode, item_type) {
        (EditorMode::Create, ItemType::Login) => "New login",
        (EditorMode::Create, ItemType::SecureNote) => "New secure note",
        (EditorMode::Edit(_), ItemType::Login) => "Edit login",
        (EditorMode::Edit(_), ItemType::SecureNote) => "Edit secure note",
    }
}

/// Stable element id for one note color swatch in the secure-note workspace.
fn note_color_element_id(color: NoteColor) -> &'static str {
    match color {
        NoteColor::Neutral => "note-color-neutral",
        NoteColor::Blue => "note-color-blue",
        NoteColor::Purple => "note-color-purple",
        NoteColor::Orange => "note-color-orange",
        NoteColor::Gold => "note-color-gold",
        NoteColor::Green => "note-color-green",
    }
}

/// Stable element id for one tag suggestion chip. Tags are short free text
/// (max 32 chars, trimmed), so the id folds the tag to lowercase with
/// non-alphanumeric characters as dashes — stable enough for tests and
/// tooling to address a suggestion by name.
fn note_tag_id(prefix: &str, tag: &str) -> SharedString {
    let sanitized = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    SharedString::from(format!("{prefix}-{sanitized}"))
}

fn note_tag_suggestion_element_id(tag: &str) -> SharedString {
    note_tag_id("note-tag-suggestion", tag)
}

fn note_tag_filter_option_element_id(tag: &str) -> SharedString {
    note_tag_id("note-tag-filter-option", tag)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NoteMarkdownFormat {
    Bold,
    Italic,
    List,
    Code,
    CopyBlock,
    LockedCopyBlock,
}

#[derive(Debug, Eq, PartialEq)]
struct NoteMarkdownEdit {
    text: String,
    selection: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NotePreviewBlockKind {
    Heading,
    ListItem,
    Code,
    Paragraph,
    CopyBlock,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NotePreviewBlock {
    kind: NotePreviewBlockKind,
    spans: Vec<NotePreviewSpan>,
    label: Option<String>,
    locked: bool,
    copy_text: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NotePreviewSpanStyle {
    Plain,
    Bold,
    Italic,
    Code,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NotePreviewSpan {
    style: NotePreviewSpanStyle,
    text: String,
}

#[cfg(test)]
impl NotePreviewBlock {
    fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

fn secure_note_has_markdown(value: &str) -> bool {
    value.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("# ")
            || trimmed.starts_with("## ")
            || trimmed.starts_with("### ")
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("```")
            || trimmed.starts_with(":::copy")
    }) || value.contains("**")
        || value.contains('`')
        || (value.contains('[') && value.contains("]("))
}

fn secure_note_markdown_preview_blocks(value: &str) -> Vec<NotePreviewBlock> {
    let mut blocks = Vec::new();
    let mut lines = value.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "```" {
            continue;
        }
        if let Some((locked, label)) = parse_copy_block_opening(trimmed) {
            let mut content = Vec::new();
            let mut closed = false;
            for inner in lines.by_ref() {
                if inner.trim() == ":::" {
                    closed = true;
                    break;
                }
                content.push(inner);
            }
            if closed {
                let copy_text = content.join("\n");
                blocks.push(NotePreviewBlock {
                    kind: NotePreviewBlockKind::CopyBlock,
                    spans: parse_inline_markdown(&copy_text),
                    label,
                    locked,
                    copy_text: Some(copy_text),
                });
                continue;
            }
            blocks.push(plain_note_preview_block(trimmed));
            for inner in content {
                blocks.push(plain_note_preview_block(inner.trim()));
            }
            continue;
        }

        let (kind, text) = if let Some(text) = trimmed.strip_prefix("### ") {
            (NotePreviewBlockKind::Heading, text)
        } else if let Some(text) = trimmed.strip_prefix("## ") {
            (NotePreviewBlockKind::Heading, text)
        } else if let Some(text) = trimmed.strip_prefix("# ") {
            (NotePreviewBlockKind::Heading, text)
        } else if let Some(text) = trimmed.strip_prefix("- ") {
            (NotePreviewBlockKind::ListItem, text)
        } else if let Some(text) = trimmed.strip_prefix("* ") {
            (NotePreviewBlockKind::ListItem, text)
        } else if trimmed.starts_with("    ") {
            (NotePreviewBlockKind::Code, trimmed)
        } else {
            (NotePreviewBlockKind::Paragraph, trimmed)
        };
        blocks.push(NotePreviewBlock {
            kind,
            spans: parse_inline_markdown(text),
            label: None,
            locked: false,
            copy_text: None,
        });
    }
    blocks
}

fn parse_copy_block_opening(line: &str) -> Option<(bool, Option<String>)> {
    let (locked, rest) = if let Some(rest) = line.strip_prefix(":::copy-locked") {
        (true, rest)
    } else if let Some(rest) = line.strip_prefix(":::copy") {
        (false, rest)
    } else {
        return None;
    };
    let label = rest.trim();
    Some((locked, (!label.is_empty()).then(|| label.to_owned())))
}

fn plain_note_preview_block(text: &str) -> NotePreviewBlock {
    NotePreviewBlock {
        kind: NotePreviewBlockKind::Paragraph,
        spans: parse_inline_markdown(text),
        label: None,
        locked: false,
        copy_text: None,
    }
}

fn parse_inline_markdown(value: &str) -> Vec<NotePreviewSpan> {
    let mut spans = Vec::new();
    let mut remaining = value;
    while !remaining.is_empty() {
        let markers = [
            (remaining.find("**"), "**", NotePreviewSpanStyle::Bold),
            (remaining.find('`'), "`", NotePreviewSpanStyle::Code),
            (remaining.find('*'), "*", NotePreviewSpanStyle::Italic),
        ];
        let Some((start, marker, style)) = markers
            .into_iter()
            .filter_map(|(index, marker, style)| index.map(|index| (index, marker, style)))
            .min_by_key(|(index, _, _)| *index)
        else {
            push_note_preview_span(&mut spans, NotePreviewSpanStyle::Plain, remaining);
            break;
        };
        if start > 0 {
            push_note_preview_span(&mut spans, NotePreviewSpanStyle::Plain, &remaining[..start]);
        }
        let content_start = start + marker.len();
        let Some(relative_end) = remaining[content_start..].find(marker) else {
            push_note_preview_span(&mut spans, NotePreviewSpanStyle::Plain, &remaining[start..]);
            break;
        };
        let content_end = content_start + relative_end;
        push_note_preview_span(&mut spans, style, &remaining[content_start..content_end]);
        remaining = &remaining[(content_end + marker.len())..];
    }
    spans
}

fn push_note_preview_span(
    spans: &mut Vec<NotePreviewSpan>,
    style: NotePreviewSpanStyle,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    spans.push(NotePreviewSpan {
        style,
        text: text.replace('[', "").replace("](", " ").replace(')', ""),
    });
}

fn apply_note_markdown_format(
    value: &str,
    selected_range: Range<usize>,
    format: NoteMarkdownFormat,
) -> NoteMarkdownEdit {
    match format {
        NoteMarkdownFormat::Bold => wrap_note_markdown_selection(value, selected_range, "**", "**"),
        NoteMarkdownFormat::Italic => wrap_note_markdown_selection(value, selected_range, "*", "*"),
        NoteMarkdownFormat::Code => wrap_note_markdown_selection(value, selected_range, "`", "`"),
        NoteMarkdownFormat::List => list_note_markdown_selection(value, selected_range),
        NoteMarkdownFormat::CopyBlock => {
            insert_note_markdown_copy_block(value, selected_range, false)
        }
        NoteMarkdownFormat::LockedCopyBlock => {
            insert_note_markdown_copy_block(value, selected_range, true)
        }
    }
}

fn wrap_note_markdown_selection(
    value: &str,
    selected_range: Range<usize>,
    prefix: &str,
    suffix: &str,
) -> NoteMarkdownEdit {
    let range = note_markdown_target_range(value, selected_range);
    let mut text = String::with_capacity(value.len() + prefix.len() + suffix.len());
    text.push_str(&value[..range.start]);
    text.push_str(prefix);
    text.push_str(&value[range.clone()]);
    text.push_str(suffix);
    text.push_str(&value[range.end..]);

    let selection_start = range.start + prefix.len();
    let selection_end = selection_start + range.len();
    NoteMarkdownEdit {
        text,
        selection: selection_start..selection_end,
    }
}

fn insert_note_markdown_copy_block(
    value: &str,
    selected_range: Range<usize>,
    locked: bool,
) -> NoteMarkdownEdit {
    let range = note_markdown_target_range(value, selected_range);
    let opening = if locked { ":::copy-locked" } else { ":::copy" };
    let selected = &value[range.clone()];
    let body = if selected.is_empty() {
        "content"
    } else {
        selected
    };
    let replacement = format!("{opening}\n{body}\n:::");

    let mut text = String::with_capacity(value.len() + replacement.len());
    text.push_str(&value[..range.start]);
    text.push_str(&replacement);
    text.push_str(&value[range.end..]);

    let body_start = range.start + opening.len() + 1;
    NoteMarkdownEdit {
        text,
        selection: body_start..(body_start + body.len()),
    }
}

fn list_note_markdown_selection(value: &str, selected_range: Range<usize>) -> NoteMarkdownEdit {
    let range = note_markdown_target_range(value, selected_range);
    let line_start = value[..range.start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end = value[range.end..]
        .find('\n')
        .map_or(value.len(), |index| range.end + index);
    let block = &value[line_start..line_end];
    let line_count = block.split('\n').count().max(1);
    let mut listed = String::with_capacity(block.len() + (line_count * 2));
    for (index, line) in block.split('\n').enumerate() {
        if index > 0 {
            listed.push('\n');
        }
        listed.push_str("- ");
        listed.push_str(line);
    }

    let mut text = String::with_capacity(value.len() + (line_count * 2));
    text.push_str(&value[..line_start]);
    text.push_str(&listed);
    text.push_str(&value[line_end..]);

    NoteMarkdownEdit {
        text,
        selection: (range.start + 2)..(range.end + (line_count * 2)),
    }
}

fn note_markdown_target_range(value: &str, selected_range: Range<usize>) -> Range<usize> {
    let start = clamp_note_markdown_offset(value, selected_range.start);
    let end = clamp_note_markdown_offset(value, selected_range.end);
    if start != end {
        return start.min(end)..start.max(end);
    }

    let cursor = start;
    let before = value[..cursor]
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(index, character)| index + character.len_utf8());
    let after = value[cursor..]
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .map_or(value.len(), |(index, _)| cursor + index);
    before..after
}

fn clamp_note_markdown_offset(value: &str, offset: usize) -> usize {
    let mut offset = offset.min(value.len());
    while offset > 0 && !value.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// What the picker's "Favicon del sitio" row is doing right now. Only an
/// explicit click moves it out of `Idle` — nothing here ever starts on its own.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FaviconFetchStatus {
    #[default]
    Idle,
    Loading,
    Failed,
}

/// Shown inline in the popover when a fetch comes back empty-handed: the
/// popover stays open and the item's icon is left exactly as it was.
pub(crate) const FAVICON_FETCH_ERROR: &str = "Couldn't fetch a favicon for that address.";

pub(crate) struct FaviconPickerState {
    /// The URL typed into the row's inline field when the item has no saved
    /// URI. It feeds the fetch only — it is never added to `item.uris`.
    pub(crate) url_input: Entity<InputState>,
    pub(crate) status: FaviconFetchStatus,
    pub(crate) last_auto_fetch_uri: Option<String>,
}

pub(crate) struct GeneratorPopoverState {
    pub(crate) open: bool,
    pub(crate) length: usize,
    pub(crate) classes: CharClasses,
    pub(crate) generated: Option<Password>,
    pub(crate) length_input: Entity<InputState>,
}

pub(crate) struct ItemEditorState {
    /// Stamped once per opening and never reused, so an async reply can prove
    /// it is still talking to the editor that sent it.
    pub(crate) id: EditorId,
    pub(crate) mode: EditorMode,
    pub(crate) item_type: ItemType,
    pub(crate) icon: IconChoice,
    /// The secure note's accent color (`locker.pen` "Note Color Field").
    pub(crate) note_color: NoteColor,
    /// Whether the loaded item is favorited. No editor UI toggles this — it
    /// is only ever set/cleared from a list row — but it is mirrored here so
    /// editing/saving an item never silently un-favorites it.
    pub(crate) favorite: bool,
    /// Free-form secure-note tags shown as chips in the Note settings combobox.
    pub(crate) note_tags: Vec<String>,
    pub(crate) note_tag_input: Entity<InputState>,
    pub(crate) markdown_preview_open: bool,
    pub(crate) revealed_copy_blocks: std::collections::BTreeSet<usize>,
    pub(crate) local_icon: Option<crate::icons::LocalIconRef>,
    pub(crate) title_input: Entity<InputState>,
    pub(crate) username_input: Entity<InputState>,
    pub(crate) password_input: Entity<InputState>,
    /// Which tab the icon popover is showing (`locker.pen` `MuIQg`/`kaJQk`).
    pub(crate) picker_tab: IconPickerTab,
    /// The Custom tab's direct image URL.
    pub(crate) icon_url_input: Entity<InputState>,
    /// Why the last icon attempt failed. Upload and URL errors used to be
    /// discarded with `let _ =`, so an oversized file did nothing at all.
    pub(crate) icon_error: Option<SharedString>,
    /// One input per website row, per `locker.pen` `S38lsY`. Never empty: the
    /// field always renders at least one row, so clearing the last URL leaves
    /// an empty row rather than a gap where the field used to be.
    pub(crate) uri_inputs: Vec<Entity<InputState>>,
    pub(crate) notes_input: Entity<TextareaState>,
    pub(crate) created_at: u64,
    pub(crate) save_error: Option<SharedString>,
    pub(crate) generator: GeneratorPopoverState,
    pub(crate) favicon: FaviconPickerState,
}

/// The icon popover's two tabs (`locker.pen` `IKki7`): bundled presets, or an
/// image the user supplies by drop, browse, or URL.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum IconPickerTab {
    #[default]
    Presets,
    Custom,
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn input(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<Nox>,
    placeholder: &'static str,
    masked: bool,
) -> Entity<InputState> {
    let value = value.into();
    let entity = cx.new(|cx| {
        let state = InputState::new(window, cx).placeholder(placeholder);
        if masked { state.masked(true) } else { state }
    });
    if !value.is_empty() {
        entity.update(cx, |state, input_cx| {
            state.set_value(value, window, input_cx)
        });
    }
    entity
}

/// Everything the icon popover needs, snapshotted before its content builder
/// runs (that closure cannot read `Nox` back mid-render).
#[derive(Clone)]
struct IconPickerSnapshot {
    theme: Theme,
    locker: Entity<Nox>,
    tab: IconPickerTab,
    current_icon: IconChoice,
    offers_favicon: bool,
    favicon_loading: bool,
    has_uris: bool,
    url_input: Option<Entity<InputState>>,
    error: Option<SharedString>,
}

/// The icon picker from `locker.pen` `MuIQg`/`kaJQk`: a header, two tabs, the
/// active tab's body, and a reset action.
///
/// Trimmed against the mock in two places, both to cut ceremony rather than
/// capability: the "Done" button is gone because every choice already applies
/// on click, and the favicon action moved next to the URL field, where the
/// other network fetch lives, instead of sitting in the footer.
fn icon_picker_popover(snapshot: IconPickerSnapshot, cx: &App) -> AnyElement {
    let IconPickerSnapshot {
        theme,
        locker,
        tab,
        current_icon,
        offers_favicon,
        favicon_loading,
        has_uris,
        url_input,
        error,
    } = snapshot;

    let tab_button = |id: &'static str, label: &'static str, this_tab: IconPickerTab| {
        let active = tab == this_tab;
        let locker = locker.clone();
        // A segmented control, not two buttons: the inactive half has to melt
        // into the track, so both halves take an explicit fill instead of the
        // default variant's border-and-background chrome.
        let (fill, hover) = if active {
            (theme.pill_active, theme.pill_active)
        } else {
            (theme.inset, theme.raised)
        };
        Button::new(id)
            .custom(ButtonCustomVariant::new(cx).color(fill).hover(hover))
            .flex_1()
            .h_full()
            .rounded(px(6.))
            .on_click(move |_, _window, app| {
                locker.update(app, |locker, cx| locker.set_icon_picker_tab(this_tab, cx));
            })
            .child(
                div()
                    .text_size(px(10.))
                    .font_weight(FontWeight(if active { 650. } else { 550. }))
                    .text_color(if active { theme.text } else { theme.icon_muted })
                    .child(label),
            )
    };

    let reset_locker = locker.clone();
    div()
        .id("item-icon-picker-menu")
        .w(px(420.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .p(px(20.))
        .rounded(px(12.))
        .border_1()
        .border_color(theme.field_border)
        .bg(theme.surface)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight(650.))
                        .text_color(theme.text)
                        .child("Choose an icon"),
                )
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(theme.text_subtle)
                        .child("Used to identify this login in your vault"),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(38.))
                .flex()
                .gap(px(4.))
                .p(px(4.))
                .rounded(px(8.))
                .bg(theme.inset)
                .child(tab_button(
                    "icon-tab-presets",
                    "Presets",
                    IconPickerTab::Presets,
                ))
                .child(tab_button(
                    "icon-tab-custom",
                    "Custom",
                    IconPickerTab::Custom,
                )),
        )
        .child(match tab {
            IconPickerTab::Presets => preset_tab_body(theme, locker.clone(), current_icon),
            IconPickerTab::Custom => custom_tab_body(
                theme,
                locker.clone(),
                url_input,
                offers_favicon,
                favicon_loading,
                has_uris,
            ),
        })
        .when_some(error, |this, message| {
            this.child(
                div()
                    .text_size(px(9.5))
                    .text_color(theme.danger)
                    .child(message),
            )
        })
        .child(
            div().flex().child(
                Button::new("icon-reset-default")
                    .ghost()
                    .h(px(34.))
                    .px(px(8.))
                    .on_click(move |_, window, app| {
                        reset_locker.update(app, |locker, cx| {
                            locker.choose_item_icon(IconChoice::Default, window, cx);
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .child(
                                gpui_component::Icon::empty()
                                    .path("icons/rotate-ccw.svg")
                                    .size(px(13.))
                                    .text_color(theme.text_secondary),
                            )
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight(550.))
                                    .text_color(theme.text_secondary)
                                    .child("Reset to default"),
                            ),
                    ),
            ),
        )
        .into_any_element()
}

/// Presets tab (`locker.pen` `hSozh`): five per row, the chosen one outlined
/// and check-badged so selection survives being one of twenty monochrome
/// glyphs. Each swatch is a real `Button`, so it is reachable by keyboard and
/// carries its own label.
fn preset_tab_body(theme: Theme, locker: Entity<Nox>, current: IconChoice) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(px(8.))
                        .font_weight(FontWeight(700.))
                        .text_color(theme.icon_muted)
                        .child("SELECT A PRESET"),
                )
                .child(
                    div()
                        .text_size(px(9.))
                        .text_color(theme.text_ghost)
                        .child(match current {
                            IconChoice::Preset(preset) => SharedString::from(format!(
                                "{} selected",
                                crate::icons::preset_icon_label(preset)
                            )),
                            IconChoice::Favicon => SharedString::from("Site favicon selected"),
                            IconChoice::Default => SharedString::from("Default icon"),
                        }),
                ),
        )
        .children(crate::icons::ALL_PRESETS.chunks(5).map(|row| {
            div().flex().gap(px(10.)).children(row.iter().map(|preset| {
                let preset = *preset;
                let selected = current == IconChoice::Preset(preset);
                let locker = locker.clone();
                Button::new(SharedString::from(format!("icon-preset-{preset:?}")))
                    .flex_1()
                    .h(px(54.))
                    .rounded(px(8.))
                    .bg(if selected { theme.raised } else { theme.field })
                    .border_1()
                    .border_color(if selected {
                        theme.inverse
                    } else {
                        theme.field_border
                    })
                    .tooltip(crate::icons::preset_icon_label(preset))
                    .on_click(move |_, window, app| {
                        locker.update(app, |locker, cx| {
                            locker.choose_item_icon(IconChoice::Preset(preset), window, cx);
                        });
                    })
                    .child(
                        gpui_component::Icon::empty()
                            .path(crate::icons::preset_icon_path(preset))
                            .size(px(20.))
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_muted
                            }),
                    )
            }))
        }))
        .into_any_element()
}

/// Custom tab (`locker.pen` `DU6wU`/`kIyq0`): drop a file, click to browse, or
/// paste a direct image URL. For a Login the site's own favicon sits here too —
/// it is the same "fetch an image off the network" family as the URL field, and
/// putting it behind a button is what makes that fetch an explicit act.
fn custom_tab_body(
    theme: Theme,
    locker: Entity<Nox>,
    url_input: Option<Entity<InputState>>,
    offers_favicon: bool,
    favicon_loading: bool,
    has_uris: bool,
) -> AnyElement {
    let browse_locker = locker.clone();
    let drop_locker = locker.clone();
    let apply_locker = locker.clone();
    let favicon_locker = locker.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(px(8.))
                        .font_weight(FontWeight(700.))
                        .text_color(theme.icon_muted)
                        .child("CUSTOM ICON"),
                )
                .child(
                    div()
                        .text_size(px(8.))
                        .text_color(theme.text_ghost)
                        .child("PNG · SVG · JPG"),
                ),
        )
        .child(
            div()
                .id("icon-dropzone")
                .w_full()
                .h(px(116.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .rounded(px(9.))
                .border_1()
                .border_color(theme.field_border)
                .bg(theme.inset)
                .cursor_pointer()
                .on_drop(move |paths: &gpui::ExternalPaths, window, app| {
                    let paths = paths.paths().to_vec();
                    drop_locker.update(app, |locker, cx| {
                        locker.drop_item_icon_paths(&paths, window, cx);
                    });
                })
                .on_click(move |_, window, app| {
                    browse_locker.update(app, |locker, cx| {
                        locker.choose_uploaded_item_icon(window, cx);
                    });
                })
                .child(
                    div()
                        .size(px(38.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .bg(theme.raised)
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/image-up.svg")
                                .size(px(17.))
                                .text_color(theme.text_soft),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight(600.))
                        .text_color(theme.text_soft)
                        .child("Drop an image here or browse"),
                )
                .child(
                    div()
                        .text_size(px(8.5))
                        .text_color(theme.text_ghost)
                        .child("Square images work best · 5 MB maximum"),
                ),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(px(8.))
                        .font_weight(FontWeight(700.))
                        .text_color(theme.icon_muted)
                        .child("IMAGE URL"),
                )
                .child(
                    div()
                        .text_size(px(8.5))
                        .text_color(theme.text_ghost)
                        .child("Optional"),
                ),
        )
        .when_some(url_input, |this, url_input| {
            this.child(
                div()
                    .w_full()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(11.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(theme.field_border)
                    .bg(theme.field)
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/link.svg")
                            .size(px(13.))
                            .text_color(theme.icon_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&url_input).appearance(false)),
                    )
                    .child(
                        Button::new("icon-url-apply")
                            .h(px(26.))
                            .px(px(9.))
                            .rounded(px(5.))
                            .bg(theme.pill_active)
                            .on_click(move |_, window, app| {
                                apply_locker.update(app, |locker, cx| {
                                    locker.apply_icon_url(window, cx);
                                });
                            })
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .font_weight(FontWeight(600.))
                                    .text_color(theme.text_soft)
                                    .child("Apply"),
                            ),
                    ),
            )
        })
        .when(offers_favicon, |this| {
            this.child(
                Button::new("icon-use-favicon")
                    .ghost()
                    .h(px(30.))
                    .px(px(8.))
                    .disabled(!has_uris || favicon_loading)
                    .on_click(move |_, window, app| {
                        favicon_locker.update(app, |locker, cx| {
                            locker.choose_item_icon(IconChoice::Favicon, window, cx);
                            locker.fetch_item_favicon(window, cx);
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .child(
                                gpui_component::Icon::empty()
                                    .path("icons/globe.svg")
                                    .size(px(13.))
                                    .text_color(theme.text_secondary),
                            )
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight(550.))
                                    .text_color(theme.text_secondary)
                                    .child(if favicon_loading {
                                        "Fetching the site icon…"
                                    } else if has_uris {
                                        "Use the site's favicon"
                                    } else {
                                        "Add a website first to use its favicon"
                                    }),
                            ),
                    ),
            )
        })
        .into_any_element()
}

/// The websites field body from `locker.pen` `S38lsY`: one 42px row per URL,
/// each with a globe prefix and an `x` that removes it, then the "Add website"
/// action. Shared by the login workspace and the generic editor sheet so both
/// surfaces stay on the one `uri_inputs` model.
fn website_rows(theme: Theme, locker: Entity<Nox>, inputs: Vec<Entity<InputState>>) -> AnyElement {
    let add_locker = locker.clone();
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(8.))
        .children(inputs.into_iter().enumerate().map(|(index, uri)| {
            let remove_locker = locker.clone();
            div()
                .w_full()
                .h(px(42.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(9.))
                .px(px(11.))
                .rounded(px(7.))
                .border_1()
                .border_color(theme.field_border)
                .bg(theme.field)
                .child(
                    gpui_component::Icon::empty()
                        .path("icons/globe.svg")
                        .size(px(14.))
                        .text_color(theme.icon_muted),
                )
                .child(div().flex_1().child(Input::new(&uri).appearance(false)))
                .child(
                    div()
                        .id(SharedString::from(format!("remove-website-{index}")))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, app| {
                            remove_locker.update(app, |locker, cx| {
                                locker.remove_website_row(index, window, cx);
                            });
                        })
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/x.svg")
                                .size(px(14.))
                                .text_color(theme.text_subtle),
                        ),
                )
        }))
        .child(
            Button::new("add-website")
                .ghost()
                .h(px(28.))
                .px(px(6.))
                .on_click(move |_, window, app| {
                    add_locker.update(app, |locker, cx| {
                        locker.add_website_row(window, cx);
                    });
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/plus.svg")
                                .size(px(14.))
                                .text_color(theme.text_secondary),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight(600.))
                                .text_color(theme.text_secondary)
                                .child("Add website"),
                        ),
                ),
        )
        .into_any_element()
}

/// One website row's input. The placeholder is the design's `https://`
/// (`locker.pen` `Snrrl`), not a per-row hint, so every row reads the same.
fn uri_input(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<Nox>,
) -> Entity<InputState> {
    input(value, window, cx, "https://", false)
}

fn textarea(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<Nox>,
    placeholder: &'static str,
) -> Entity<TextareaState> {
    let value = value.into();
    let entity = cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .rows(4)
    });
    if !value.is_empty() {
        entity.update(cx, |state, input_cx| {
            state.set_value(value, window, input_cx)
        });
    }
    entity
}

impl ItemEditorState {
    pub(crate) fn for_create(window: &mut Window, cx: &mut Context<Nox>) -> Self {
        let title_input = input("", window, cx, "Title", false);
        let username_input = input("", window, cx, "Username", false);
        let password_input = input("", window, cx, "Password", true);
        let uri_inputs = vec![uri_input("", window, cx)];
        let notes_input = textarea("", window, cx, "Notes");
        let length_input = input("20", window, cx, "Length", false);
        let favicon_url_input = input("", window, cx, "https://example.com", false);
        let icon_url_input = input("", window, cx, "https://example.com/icon.png", false);
        let note_tag_input = input("", window, cx, "Add tag...", false);
        Self {
            id: EditorId::next(),
            mode: EditorMode::Create,
            item_type: ItemType::Login,
            icon: IconChoice::Default,
            note_color: NoteColor::default(),
            favorite: false,
            note_tags: Vec::new(),
            note_tag_input,
            markdown_preview_open: false,
            revealed_copy_blocks: std::collections::BTreeSet::new(),
            local_icon: None,
            title_input,
            username_input,
            password_input,
            picker_tab: IconPickerTab::default(),
            icon_url_input,
            icon_error: None,
            uri_inputs,
            notes_input,
            created_at: now_millis(),
            save_error: None,
            generator: GeneratorPopoverState {
                open: false,
                length: 20,
                classes: CharClasses::ALL,
                generated: None,
                length_input,
            },
            favicon: FaviconPickerState {
                url_input: favicon_url_input,
                status: FaviconFetchStatus::Idle,
                last_auto_fetch_uri: None,
            },
        }
    }

    /// The item's URI lines as the editor currently holds them — the same list
    /// `payload` saves, and the candidates an explicit favicon fetch walks.
    pub(crate) fn uris(&self, cx: &App) -> Vec<String> {
        self.uri_inputs
            .iter()
            .map(|input| input.read(cx).value().trim().to_owned())
            .filter(|uri| !uri.is_empty())
            .collect()
    }

    pub(crate) fn local_icon_key(&self) -> String {
        match self.mode {
            EditorMode::Create => crate::icons::editor_key(self.id.0),
            EditorMode::Edit(item_id) => crate::icons::item_key(item_id),
        }
    }

    #[cfg(test)]
    pub(crate) fn payload_for_test(&self, cx: &App) -> ItemPayload {
        let item_type = self.item_type;
        let username = if item_type == ItemType::Login {
            self.username_input.read(cx).value().to_string()
        } else {
            String::new()
        };
        let password = if item_type == ItemType::Login {
            self.password_input.read(cx).value().to_string()
        } else {
            String::new()
        };
        let uris = if item_type == ItemType::Login {
            self.uris(cx)
        } else {
            Vec::new()
        };
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type,
            title: self.title_input.read(cx).value().to_string(),
            username,
            password,
            uris,
            notes: self.notes_input.read(cx).value().to_string(),
            created_at: self.created_at,
            updated_at: now_millis(),
            icon: self.icon,
            note_color: self.note_color,
            note_tags: normalize_note_tags(&self.note_tags),
            favorite: self.favorite,
        }
    }

    pub(crate) fn for_edit(
        item_id: ItemId,
        vault: &Vault,
        window: &mut Window,
        cx: &mut Context<Nox>,
    ) -> Result<Self, VaultError> {
        let payload = vault.get_item(item_id)?.ok_or(VaultError::ItemNotFound)?;
        Ok(Self::from_payload(
            EditorMode::Edit(item_id),
            payload,
            window,
            cx,
        ))
    }

    fn from_payload(
        mode: EditorMode,
        payload: ItemPayload,
        window: &mut Window,
        cx: &mut Context<Nox>,
    ) -> Self {
        let mut editor = Self::for_create(window, cx);
        editor.mode = mode;
        editor.item_type = payload.item_type;
        editor.icon = payload.icon;
        editor.note_color = payload.note_color;
        editor.favorite = payload.favorite;
        editor.note_tags = payload.note_tags;
        editor.markdown_preview_open = false;
        editor.revealed_copy_blocks.clear();
        editor.created_at = payload.created_at;
        editor.title_input.update(cx, |state, input_cx| {
            state.set_value(payload.title, window, input_cx)
        });
        editor.username_input.update(cx, |state, input_cx| {
            state.set_value(payload.username, window, input_cx)
        });
        editor.password_input.update(cx, |state, input_cx| {
            state.set_value(payload.password, window, input_cx)
        });
        // One row per saved URL, and a single empty row when the item has none
        // — the field is never rendered without a row to type into.
        editor.uri_inputs = if payload.uris.is_empty() {
            vec![uri_input("", window, cx)]
        } else {
            payload
                .uris
                .iter()
                .map(|uri| uri_input(uri.clone(), window, cx))
                .collect()
        };
        editor.notes_input.update(cx, |state, input_cx| {
            state.set_value(payload.notes, window, input_cx)
        });
        editor
    }

    fn payload(&self, window: &mut Window, cx: &mut Context<Nox>) -> ItemPayload {
        let item_type = self.item_type;
        let title = self.title_input.read(cx).value().to_string();
        let username = if item_type == ItemType::Login {
            self.username_input.read(cx).value().to_string()
        } else {
            String::new()
        };
        let password = if item_type == ItemType::Login {
            self.password_input.read(cx).value().to_string()
        } else {
            String::new()
        };
        let uris = if item_type == ItemType::Login {
            self.uris(cx)
        } else {
            Vec::new()
        };
        let _ = window;
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type,
            title,
            username,
            password,
            uris,
            notes: self.notes_input.read(cx).value().to_string(),
            created_at: self.created_at,
            updated_at: now_millis(),
            icon: self.icon,
            note_color: self.note_color,
            note_tags: normalize_note_tags(&self.note_tags),
            favorite: self.favorite,
        }
    }
}

/// A crude 0–4 length+variety heuristic, not a real entropy estimate —
/// upgrade to something like zxcvbn if that's ever asked for. Matches the
/// same spirit as `vault_list::is_weak_password`, just live/granular instead
/// of a single weak/not-weak cutoff.
fn password_strength_score(password: &str) -> usize {
    if password.is_empty() {
        return 0;
    }
    let len = password.chars().count();
    let class_count = [
        password.chars().any(|c| c.is_ascii_lowercase()),
        password.chars().any(|c| c.is_ascii_uppercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_ascii_alphanumeric()),
    ]
    .into_iter()
    .filter(|met| *met)
    .count();
    let mut score = 1;
    if len >= 10 {
        score += 1;
    }
    if len >= 14 {
        score += 1;
    }
    if class_count >= 3 {
        score += 1;
    }
    score.min(4)
}

/// Whether this draft password already belongs to a saved login — checked
/// against the real vault, not a fabricated signal.
fn password_is_reused(password: &str, items: &[(ItemId, ItemPayload)]) -> bool {
    !password.is_empty()
        && items.iter().any(|(_, payload)| {
            payload.item_type == ItemType::Login && payload.password == password
        })
}

impl Nox {
    pub(crate) fn uses_secure_note_workspace(&self) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        match session.active_view {
            // Home has no type filter of its own — its "+ Add item" menu
            // opens a create editor directly and expects the same full-page
            // workspace Secure Notes gives its own creation flow.
            ActiveView::SecureNotes | ActiveView::Home => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Create | EditorMode::Edit(_),
                    item_type: ItemType::SecureNote,
                    ..
                })
            ),
            // All items reuses the full-page editor for edits only; creation
            // there keeps the generic Sheet.
            ActiveView::AllItems => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Edit(_),
                    item_type: ItemType::SecureNote,
                    ..
                })
            ),
            _ => false,
        }
    }

    /// A dedicated full-page login flow instead of the generic Sheet.
    pub(crate) fn uses_login_workspace(&self) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        match session.active_view {
            // Home has no type filter of its own — its "+ Add item" menu
            // opens a create editor directly and expects the same full-page
            // workspace Logins gives its own creation flow.
            ActiveView::Logins | ActiveView::Home => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Create | EditorMode::Edit(_),
                    item_type: ItemType::Login,
                    ..
                })
            ),
            // All items reuses the full-page editor for edits only; creation
            // there keeps the generic Sheet.
            ActiveView::AllItems => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Edit(_),
                    item_type: ItemType::Login,
                    ..
                })
            ),
            _ => false,
        }
    }

    /// Set the secure note's accent color (`locker.pen` "Note Color Field").
    pub(crate) fn choose_note_color(&mut self, color: NoteColor, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            editor.note_color = color;
            cx.notify();
        }
    }

    pub(crate) fn add_note_tag(&mut self, tag: impl AsRef<str>, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            let mut tags = editor.note_tags.clone();
            tags.push(tag.as_ref().to_owned());
            editor.note_tags = normalize_note_tags(tags);
            cx.notify();
        }
    }

    /// Tags already saved on other secure notes in this vault, as a stable
    /// click-to-add suggestion list. With no query it shows the top three tags
    /// by usage, then recency; while typing it filters that same list by the
    /// input text. Logins never contribute — tags are a secure-note concept.
    pub(crate) fn note_tag_suggestions(&self) -> Vec<String> {
        self.note_tag_suggestions_for_query("")
    }

    pub(crate) fn note_tag_suggestions_for_query(&self, query: &str) -> Vec<String> {
        let Some(session) = self.session() else {
            return Vec::new();
        };
        let on_editor: std::collections::HashSet<String> = self
            .item_editor()
            .map(|editor| {
                editor
                    .note_tags
                    .iter()
                    .map(|tag| tag.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let query = query.trim().to_lowercase();
        let mut stats: std::collections::HashMap<String, (String, usize, u64)> =
            std::collections::HashMap::new();
        for (_, payload) in session
            .list
            .items
            .iter()
            .filter(|(_, payload)| payload.item_type == ItemType::SecureNote)
        {
            for tag in normalize_note_tags(&payload.note_tags) {
                let key = tag.to_lowercase();
                if on_editor.contains(&key) || (!query.is_empty() && !key.contains(&query)) {
                    continue;
                }
                let entry = stats.entry(key).or_insert((tag, 0, 0));
                entry.1 += 1;
                entry.2 = entry.2.max(payload.updated_at);
            }
        }
        let mut suggestions = stats.into_values().collect::<Vec<_>>();
        suggestions.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| left.0.to_lowercase().cmp(&right.0.to_lowercase()))
        });
        suggestions
            .into_iter()
            .take(3)
            .map(|(tag, _, _)| tag)
            .collect()
    }

    /// Click-to-add from the suggestions list. The same normalization as
    /// typing applies, so a suggestion that is already a chip (or a case
    /// variant of one) is a no-op.
    pub(crate) fn select_note_tag_suggestion(&mut self, tag: &str, cx: &mut Context<Self>) {
        self.add_note_tag(tag, cx);
    }

    pub(crate) fn remove_note_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            let key = tag.to_lowercase();
            editor
                .note_tags
                .retain(|existing| existing.to_lowercase() != key);
            cx.notify();
        }
    }

    pub(crate) fn commit_note_tag_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag) = self
            .item_editor()
            .map(|editor| editor.note_tag_input.read(cx).value().to_string())
        else {
            return;
        };
        self.add_note_tag(tag, cx);
        if let Some(editor) = self.item_editor() {
            editor.note_tag_input.update(cx, |state, input_cx| {
                state.set_value("".to_owned(), window, input_cx)
            });
        }
    }

    fn format_secure_note_markdown(
        &mut self,
        format: NoteMarkdownFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(notes_input) = self.item_editor().map(|editor| editor.notes_input.clone()) else {
            return;
        };
        notes_input.update(cx, |state, input_cx| {
            let edit =
                apply_note_markdown_format(state.value().as_ref(), state.selected_range(), format);
            state.replace_all(edit.text, window, input_cx);
            state.set_selected_range(edit.selection, input_cx);
            state.focus(window, input_cx);
        });
        cx.notify();
    }

    fn toggle_secure_note_markdown_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            let has_markdown =
                secure_note_has_markdown(editor.notes_input.read(cx).value().as_ref());
            editor.markdown_preview_open = has_markdown && !editor.markdown_preview_open;
            cx.notify();
        }
    }

    fn toggle_secure_note_copy_block_reveal(&mut self, block_index: usize, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            if !editor.revealed_copy_blocks.insert(block_index) {
                editor.revealed_copy_blocks.remove(&block_index);
            }
            cx.notify();
        }
    }

    /// Applies an icon choice from the picker and marks the field dirty the
    /// same way any other editor field change does — picking an icon is
    /// part of editing the item, not a separate save.
    pub(crate) fn choose_item_icon(
        &mut self,
        icon: IconChoice,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Picking Default or a preset must actually replace a previously
        // chosen local image (an upload or a fetched favicon) — otherwise
        // `resolve_item_icon`'s local-selection lookup keeps preferring the
        // old bytes forever, making the pick a silent no-op. `Favicon` is
        // the one choice a local selection legitimately backs, so it is the
        // one case that keeps it.
        let clears_local_image = !matches!(icon, IconChoice::Favicon);
        let local_key = clears_local_image
            .then(|| self.item_editor().map(|editor| editor.local_icon_key()))
            .flatten();
        if let Some(session) = self.session_mut()
            && let Some(editor) = session.item_editor.as_mut()
        {
            editor.icon = icon;
            if clears_local_image {
                editor.local_icon = None;
            }
        }
        if let Some(local_key) = local_key {
            self.clear_local_icon_selection(&local_key);
        }
        cx.notify();
    }

    pub(crate) fn icon_picker_offers_favicon(&self) -> bool {
        self.session()
            .and_then(|session| session.item_editor.as_ref())
            .is_some_and(|editor| editor.item_type == ItemType::Login)
    }

    pub(crate) fn favicon_fetch_status(&self) -> FaviconFetchStatus {
        self.item_editor()
            .map_or(FaviconFetchStatus::Idle, |editor| editor.favicon.status)
    }

    pub(crate) fn favicon_fetch_error(&self) -> Option<&'static str> {
        (self.favicon_fetch_status() == FaviconFetchStatus::Failed).then_some(FAVICON_FETCH_ERROR)
    }

    /// Candidate URIs for one explicit fetch: the item's own URI lines, or —
    /// when it has none — only the URL typed into the picker's inline field.
    /// That typed URL is used for the fetch and nothing else; it never joins
    /// `item.uris`, which the user edits through the URI field itself.
    fn favicon_fetch_candidates(&self, cx: &App) -> Vec<String> {
        let Some(editor) = self.item_editor() else {
            return Vec::new();
        };
        // The item's own websites are the only candidates now. The picker's
        // old typed-URL field is gone: a URL worth fetching from is a website
        // worth saving, so the Custom tab gates the favicon action on there
        // being a website row rather than accepting a throwaway URL.
        editor.uris(cx)
    }

    /// Runs the one favicon fetch the user just asked for. Nothing here starts
    /// without that click — no background scan, no fetch while typing a URL.
    pub(crate) fn website_field_blurred(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.icon_picker_offers_favicon() {
            return;
        }
        let Some(editor) = self.item_editor() else {
            return;
        };
        let Some(uri) = editor.uris(cx).first().cloned() else {
            return;
        };
        if editor.favicon.last_auto_fetch_uri.as_deref() == Some(uri.as_str()) {
            return;
        }
        if let Some(editor) = self.item_editor_mut() {
            editor.favicon.last_auto_fetch_uri = Some(uri);
        }
        self.fetch_item_favicon(window, cx);
    }

    /// Append an empty website row (`locker.pen` `j0b4vA`). The new row gets
    /// its own blur listener, so a URL typed into it still triggers the
    /// favicon auto-fetch that the first row would have.
    pub(crate) fn add_website_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_input = uri_input("", window, cx);
        let focus = new_input.focus_handle(cx);
        if let Some(editor) = self.item_editor_mut() {
            editor.uri_inputs.push(new_input);
        } else {
            return;
        }
        self.register_website_blur_listener(focus, window, cx);
        cx.notify();
    }

    /// Remove one website row. The last remaining row is cleared instead of
    /// removed, so the field always keeps a row to type into.
    pub(crate) fn remove_website_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.item_editor_mut() else {
            return;
        };
        if editor.uri_inputs.len() <= 1 {
            let Some(input) = editor.uri_inputs.first().cloned() else {
                return;
            };
            input.update(cx, |state, input_cx| {
                state.set_value("", window, input_cx);
            });
        } else if index < editor.uri_inputs.len() {
            editor.uri_inputs.remove(index);
        }
        cx.notify();
    }

    pub(crate) fn set_icon_picker_tab(&mut self, tab: IconPickerTab, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            editor.picker_tab = tab;
            editor.icon_error = None;
        }
        cx.notify();
    }

    fn set_icon_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            editor.icon_error = Some(message.into());
        }
        cx.notify();
    }

    /// Accept a file dropped on the Custom tab's dropzone. Same path as the
    /// browse dialog — one image, size-checked before it is read.
    pub(crate) fn drop_item_icon_paths(
        &mut self,
        paths: &[std::path::PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = paths.first() else {
            return;
        };
        let Ok(metadata) = std::fs::metadata(path) else {
            self.set_icon_error("Could not read that file.", cx);
            return;
        };
        if metadata.len() > crate::icons::MAX_LOCAL_IMAGE_BYTES as u64 {
            self.set_icon_error(crate::icons::LocalIconError::TooLarge.to_string(), cx);
            return;
        }
        let Ok(bytes) = std::fs::read(path) else {
            self.set_icon_error("Could not read that file.", cx);
            return;
        };
        self.apply_icon_bytes(&bytes, window, cx);
    }

    /// Store bytes as this item's icon, surfacing any rejection instead of
    /// swallowing it.
    fn apply_icon_bytes(&mut self, bytes: &[u8], window: &mut Window, cx: &mut Context<Self>) {
        match self.upload_item_icon_bytes(bytes, window, cx) {
            Ok(()) => {
                if let Some(editor) = self.item_editor_mut() {
                    editor.icon_error = None;
                }
                cx.notify();
            }
            Err(error) => self.set_icon_error(error.to_string(), cx),
        }
    }

    /// Fetch the image the user pasted into the Custom tab and use it. The
    /// request runs through `favicon`'s guard rails, and the reply may only
    /// touch the editor that asked for it.
    pub(crate) fn apply_icon_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((editor_id, url)) = self.item_editor().map(|editor| {
            (
                editor.id,
                editor.icon_url_input.read(cx).value().trim().to_owned(),
            )
        }) else {
            return;
        };
        if url.is_empty() {
            return;
        }
        if let Some(editor) = self.item_editor_mut() {
            editor.icon_error = None;
        }
        cx.notify();
        let client = cx.http_client();
        cx.spawn_in(window, async move |this, cx| {
            let fetched =
                cx.background_executor()
                    .spawn(async move {
                        crate::favicon::fetch_image_from_url(client.as_ref(), &url).await
                    })
                    .await;
            let _ = cx.update(|window, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |locker, cx| {
                        if !locker
                            .item_editor()
                            .is_some_and(|editor| editor.id == editor_id)
                        {
                            return;
                        }
                        match fetched {
                            Ok(bytes) => locker.apply_icon_bytes(&bytes, window, cx),
                            Err(_) => locker
                                .set_icon_error("Couldn't load an image from that address.", cx),
                        }
                    });
                }
            });
        })
        .detach();
    }

    pub(crate) fn choose_uploaded_item_icon(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor_id) = self.item_editor().map(|editor| editor.id) else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose item icon".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = receiver
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|mut paths| if paths.len() == 1 { paths.pop() } else { None });
            let Some(path) = path else {
                return;
            };
            // Check the on-disk size before reading any bytes — an oversized
            // file is rejected the same way (silently, no icon change) either
            // way, but this way a multi-gigabyte pick never gets read into
            // memory first just to be thrown away by `validate_local_image`.
            let Ok(metadata) = std::fs::metadata(&path) else {
                return;
            };
            let oversized = metadata.len() > crate::icons::MAX_LOCAL_IMAGE_BYTES as u64;
            let bytes = if oversized {
                Vec::new()
            } else {
                match std::fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(_) => return,
                }
            };
            let _ = cx.update(|window, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |locker, cx| {
                        if !locker
                            .item_editor()
                            .is_some_and(|editor| editor.id == editor_id)
                        {
                            return;
                        }
                        // An oversized pick used to return silently, so nothing
                        // happened at all and the user had no idea why.
                        if oversized {
                            locker.set_icon_error(
                                crate::icons::LocalIconError::TooLarge.to_string(),
                                cx,
                            );
                        } else {
                            locker.apply_icon_bytes(&bytes, window, cx);
                        }
                    });
                }
            });
        })
        .detach();
    }

    pub(crate) fn upload_item_icon_bytes(
        &mut self,
        bytes: &[u8],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::icons::LocalIconError> {
        let data_dir = self.data_dir.clone();
        let Some(editor) = self.item_editor_mut() else {
            return Ok(());
        };
        let local_key = editor.local_icon_key();
        let selection = crate::icons::cache_local_icon(&data_dir, &local_key, bytes)?;
        editor.local_icon = Some(selection.clone());
        editor.favicon.status = FaviconFetchStatus::Idle;
        self.record_local_icon_selection(selection);
        cx.notify();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn editor_avatar_is_before_name(&self) -> bool {
        self.item_editor().is_some()
    }

    pub(crate) fn fetch_item_favicon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.icon_picker_offers_favicon() {
            return;
        }
        // Whose fetch this is. The reply is only allowed to touch this exact
        // editor, however long it takes to arrive.
        let Some(editor_id) = self.item_editor().map(|editor| editor.id) else {
            return;
        };
        let candidates = self.favicon_fetch_candidates(cx);
        if candidates.is_empty() {
            self.finish_favicon_fetch(editor_id, None, window, cx);
            return;
        }
        let client = cx.http_client();
        let data_dir = self.data_dir.clone();
        // Captured now, not re-derived when the reply lands: by then this may
        // be a different opening entirely, and a still-unsaved Create editor's
        // key depends on this exact `editor_id`.
        let local_key = self
            .item_editor()
            .map(|editor| editor.local_icon_key())
            .unwrap_or_else(|| crate::icons::editor_key(0));
        if let Some(editor) = self.item_editor_mut() {
            editor.favicon.status = FaviconFetchStatus::Loading;
        }
        cx.notify();
        // Same shape as `create_vault`/`unlock_vault` (app.rs) and every
        // `backup.rs` task: the network and disk work runs on
        // `cx.background_executor()`, off GPUI's foreground executor;
        // `this: WeakEntity<Nox>` is upgraded before touching state back on the
        // foreground thread.
        cx.spawn_in(window, async move |this, cx| {
            let local_icon = cx
                .background_executor()
                .spawn(async move {
                    // One candidate per call: `fetch_favicon` returns bytes but
                    // not *which* URI produced them, and the cache is keyed by
                    // host — handing it the whole list would file a later URI's
                    // icon under the first parseable host, where the resolver
                    // would never find it.
                    for uri in candidates {
                        let Some(host) = crate::favicon::extract_host(&uri) else {
                            continue;
                        };
                        if let Ok(bytes) = crate::favicon::fetch_favicon(
                            client.as_ref(),
                            std::slice::from_ref(&uri),
                        )
                        .await
                        {
                            // Best-effort compatibility cache under the host key;
                            // what the editor actually renders is the
                            // content-addressed local selection cached next.
                            let _ = crate::favicon::cache_favicon(&data_dir, &host, &bytes);
                            if let Ok(selection) =
                                crate::favicon::cache_favicon_for_key(&data_dir, &local_key, &bytes)
                            {
                                return Some(selection);
                            }
                        }
                    }
                    None
                })
                .await;
            let _ = cx.update(|window, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |locker, cx| {
                        locker.finish_favicon_fetch(editor_id, local_icon, window, cx);
                    });
                }
            });
        })
        .detach();
    }

    /// On success the item moves to `IconChoice::Favicon`; on failure the icon
    /// is left exactly as it was and the row reports it inline, so the popover
    /// stays usable for a retry or another pick.
    ///
    /// `editor_id` is the editor that asked. A fetch outlives its editor easily
    /// — cancelled, saved, or swapped for another item while the request is in
    /// flight — and the editor sitting there when it answers may well be a
    /// Secure Note, which has no favicon at all. A reply that no longer matches
    /// its requester is dropped rather than applied to whoever is open now.
    fn finish_favicon_fetch(
        &mut self,
        editor_id: EditorId,
        local_icon: Option<crate::icons::LocalIconRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let still_the_requester = self
            .item_editor()
            .is_some_and(|editor| editor.id == editor_id && editor.item_type == ItemType::Login);
        if !still_the_requester {
            return;
        }
        let cached = local_icon.is_some();
        if let Some(selection) = local_icon {
            // Set before `choose_item_icon` so the picker's trigger and rows
            // resolve the new bytes on the very same render pass that flips
            // the choice to `Favicon`, instead of a stale render in between.
            if let Some(editor) = self.item_editor_mut() {
                editor.local_icon = Some(selection.clone());
            }
            self.record_local_icon_selection(selection);
            self.choose_item_icon(IconChoice::Favicon, window, cx);
        }
        if let Some(editor) = self.item_editor_mut() {
            editor.favicon.status = if cached {
                FaviconFetchStatus::Idle
            } else {
                FaviconFetchStatus::Failed
            };
        }
        cx.notify();
    }

    /// The item's icon, clickable to open the picker: Default, the 20
    /// presets, and — Login only — "Favicon del sitio". Secure Note gets no
    /// favicon row; it has no site identity to fetch from.
    ///
    /// `avatar` renders the login form's 64px field box from `locker.pen`
    /// `ztKOA`: the trigger *is* the box (a nested chip inside it reads as a
    /// box-in-a-box), and a failed favicon fetch paints it as `VQgVd`.
    /// Everywhere else the trigger stays the compact 28px chip.
    fn render_icon_picker(&mut self, avatar: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let data_dir = self.data_dir.clone();
        let (item_type, current_icon, note_color, _uris, _favicon_url_input) = self
            .session()
            .and_then(|session| session.item_editor.as_ref())
            .map(|editor| {
                (
                    editor.item_type,
                    editor.icon,
                    editor.note_color,
                    editor.uris(cx),
                    Some(editor.favicon.url_input.clone()),
                )
            })
            .unwrap_or((
                ItemType::Login,
                IconChoice::Default,
                NoteColor::default(),
                Vec::new(),
                None,
            ));

        // The trigger is the one spot that actually shows a chosen favicon
        // as an image — it's the direct feedback for "did my pick work?".
        let local_selection = self
            .item_editor()
            .and_then(|editor| editor.local_icon.as_ref());
        let local_key = self
            .item_editor()
            .map(|editor| editor.local_icon_key())
            .unwrap_or_else(|| crate::icons::editor_key(0));
        let resolved_icon = crate::icons::resolve_item_icon(
            item_type,
            current_icon,
            &data_dir,
            &local_key,
            local_selection,
        );
        // Drives the delete "x": it only makes sense to offer deleting an
        // uploaded/fetched image that is actually the thing on screen.
        let has_local_image = matches!(resolved_icon, crate::icons::ResolvedIcon::LocalImage(_));
        // `VQgVd`: a fetch that came back empty-handed swaps the icon for
        // `image-off` and turns the box's border red, so the avatar itself
        // carries the failure instead of only the popover.
        let fetch_failed = avatar && self.favicon_fetch_status() == FaviconFetchStatus::Failed;
        let (icon_size, image_size, image_radius, icon_color) = if avatar {
            (px(28.), px(28.), px(6.), theme.text_soft)
        } else {
            (px(14.), px(18.), px(4.), theme.text_secondary)
        };
        // The secure-note chip is the only icon preview living in the note
        // editor itself: it's the direct answer to "what will my note's icon
        // actually look like?" as the Note Color swatches are clicked, so it
        // has to carry the chosen color live rather than staying the fixed
        // neutral grey every other item's chip uses.
        let note_tint = (item_type == ItemType::SecureNote && note_color != NoteColor::Neutral)
            .then_some(note_color);
        let icon_color = note_tint.map_or(icon_color, crate::icons::note_color_hsla);
        let chip_bg = note_tint.map_or(theme.raised, crate::icons::note_color_wash_hsla);
        // The chip's ring matches the glyph stroke, not a separate accent —
        // otherwise the border reads as an unrelated color next to the icon
        // it's supposed to be framing.
        let chip_border = note_tint.map_or(theme.field_border, crate::icons::note_color_hsla);
        let trigger_child: AnyElement = if fetch_failed {
            gpui_component::Icon::empty()
                .path("icons/image-off.svg")
                .size(px(22.))
                .text_color(theme.danger)
                .into_any_element()
        } else {
            match resolved_icon {
                crate::icons::ResolvedIcon::Svg(path) => gpui_component::Icon::empty()
                    .path(path)
                    .size(icon_size)
                    .text_color(icon_color)
                    .into_any_element(),
                crate::icons::ResolvedIcon::LocalImage(path) => gpui::img(path)
                    .size(image_size)
                    .rounded(image_radius)
                    .into_any_element(),
                crate::icons::ResolvedIcon::UnavailableLocalImage => gpui_component::Icon::empty()
                    .path(crate::icons::default_type_icon(item_type))
                    .size(icon_size)
                    .text_color(icon_color)
                    .into_any_element(),
            }
        };
        let trigger = if avatar {
            Button::new("item-icon-picker-trigger")
                .size(px(64.))
                .rounded(px(14.))
                .bg(theme.field)
                .border_1()
                .border_color(if fetch_failed {
                    theme.danger
                } else {
                    theme.field_border
                })
                .child(trigger_child)
        } else if item_type == ItemType::SecureNote {
            Button::new("item-icon-picker-trigger")
                .h(px(28.))
                .px(px(9.))
                .gap(px(6.))
                .rounded(px(8.))
                .bg(chip_bg)
                .border_1()
                .border_color(chip_border)
                .child(trigger_child)
                .child(
                    div()
                        .text_size(px(9.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_secondary)
                        .child("NOTE"),
                )
        } else {
            Button::new("item-icon-picker-trigger")
                .size(px(28.))
                .rounded(px(8.))
                .bg(chip_bg)
                .border_1()
                .border_color(chip_border)
                .child(trigger_child)
        };

        let locker = cx.entity();
        let locker_for_delete = locker.clone();
        // The popover's content builder runs inside this same render pass and
        // cannot read `Nox` back, so its whole state is snapshotted here.
        let snapshot = IconPickerSnapshot {
            theme,
            locker: locker.clone(),
            tab: self
                .item_editor()
                .map_or(IconPickerTab::default(), |editor| editor.picker_tab),
            current_icon,
            offers_favicon: self.icon_picker_offers_favicon(),
            favicon_loading: self.favicon_fetch_status() == FaviconFetchStatus::Loading,
            has_uris: !self
                .item_editor()
                .map(|editor| editor.uris(cx))
                .unwrap_or_default()
                .is_empty(),
            url_input: self
                .item_editor()
                .map(|editor| editor.icon_url_input.clone()),
            error: self
                .item_editor()
                .and_then(|editor| editor.icon_error.clone())
                .or_else(|| self.favicon_fetch_error().map(SharedString::from)),
        };

        let picker = Popover::new("item-icon-picker")
            .appearance(false)
            .trigger(trigger)
            .content(move |_state, _window, cx| icon_picker_popover(snapshot.clone(), cx))
            .into_any_element();

        // The delete "x": edit-mode-and-create alike, shown only while the
        // avatar is actually rendering a local image, so there is nothing to
        // interpret when it's showing the type default, a preset, or an
        // unavailable placeholder. A direct click, no popover required —
        // per the spec's "an absolute-positioned top 'x' to delete the
        // uploaded or fetched image".
        //
        // The avatar box is the exception: `ztKOA` puts only the edit badge on
        // it, and clearing stays one click away as the picker's "Default" row.
        if has_local_image && !avatar {
            div()
                .relative()
                .child(picker)
                .child(
                    div()
                        .id("item-icon-delete")
                        .absolute()
                        .top(px(-4.))
                        .right(px(-4.))
                        .size(px(16.))
                        .rounded_full()
                        .bg(theme.danger)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, app| {
                            locker_for_delete.update(app, |locker, cx| {
                                locker.choose_item_icon(IconChoice::Default, window, cx);
                            });
                        })
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/x.svg")
                                .size(px(10.))
                                .text_color(theme.inverse),
                        ),
                )
                .into_any_element()
        } else {
            picker
        }
    }

    pub(crate) fn cancel_item_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut() {
            session.item_editor = None;
        }
        self.editor_blur_subscriptions.clear();
        window.close_sheet(cx);
        cx.notify();
    }

    /// Open the shadcn-style Sheet (drawer) that hosts the create/edit/restore form.
    ///
    /// The Sheet's content builder is invoked by `Root::render_sheet_layer`
    /// from *inside* `Nox`'s own render pass, so it cannot call
    /// `Entity::update`/`read` on `Nox` (it is already leased for that
    /// render and would panic). Instead `render_unlocked` refreshes
    /// `item_editor_sheet_cell` with freshly rendered content on every pass,
    /// and this closure just reads whatever is currently sitting in the cell.
    pub(crate) fn open_item_editor_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cell = self.item_editor_sheet_cell.clone();
        let locker_for_close = cx.entity();
        window.open_sheet(cx, move |sheet, _window, _cx| {
            let (title, content) = cell
                .borrow_mut()
                .take()
                .unwrap_or_else(|| ("Item".into(), div().into_any_element()));
            let locker_for_close = locker_for_close.clone();
            sheet
                .title(title)
                .child(content)
                .on_close(move |_, _window, app| {
                    locker_for_close.update(app, |locker, cx| {
                        if let Some(session) = locker.session_mut() {
                            session.item_editor = None;
                        }
                        cx.notify();
                    });
                })
        });
    }

    pub(crate) fn set_editor_type(&mut self, item_type: ItemType, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            editor.item_type = item_type;
            cx.notify();
        }
    }

    pub(crate) fn generate_editor_password(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor_mut() else {
            return;
        };
        if let Ok(length) = editor
            .generator
            .length_input
            .read(cx)
            .value()
            .parse::<usize>()
        {
            editor.generator.length = length.clamp(1, MAX_LENGTH);
        }
        editor.generator.generated =
            generate_password(editor.generator.length, editor.generator.classes).ok();
        cx.notify();
    }

    pub(crate) fn set_generator_class(
        &mut self,
        class: CharClasses,
        checked: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.item_editor_mut() {
            if checked {
                editor.generator.classes |= class;
            } else {
                let mut classes = CharClasses::EMPTY;
                for candidate in [
                    CharClasses::LOWER,
                    CharClasses::UPPER,
                    CharClasses::DIGITS,
                    CharClasses::SYMBOLS,
                ] {
                    if candidate != class && editor.generator.classes.contains(candidate) {
                        classes |= candidate;
                    }
                }
                editor.generator.classes = classes;
            }
            cx.notify();
        }
    }

    pub(crate) fn use_generated_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor_mut() else {
            return;
        };
        let Some(password) = editor.generator.generated.as_ref() else {
            return;
        };
        let value = String::from_utf8_lossy(password.as_bytes()).into_owned();
        editor.password_input.update(cx, |state, input_cx| {
            state.set_value(value, window, input_cx)
        });
        editor.generator.generated = None;
        editor.generator.open = false;
        cx.notify();
    }

    pub(crate) fn set_generator_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            editor.generator.open = open;
            cx.notify();
        }
    }

    pub(crate) fn save_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor() else {
            return;
        };
        if editor.title_input.read(cx).value().trim().is_empty() {
            if let Some(editor) = self.item_editor_mut() {
                editor.save_error = Some("Enter a title.".into());
            }
            cx.notify();
            return;
        }
        if editor.item_type == ItemType::SecureNote
            && editor.notes_input.read(cx).value().trim().is_empty()
        {
            if let Some(editor) = self.item_editor_mut() {
                editor.save_error = Some("Enter note content.".into());
            }
            cx.notify();
            return;
        }
        let mode = editor.mode;
        // A Create editor's local selection, if any, is still cached under its
        // throwaway editor key — captured here so it can be re-keyed to the
        // item's real key once `create_item` returns one below.
        let pending_local_icon = editor.local_icon.clone();
        let payload = editor.payload(window, cx);
        let result = match (&mut self.state, mode) {
            (AppState::Unlocked(session), EditorMode::Create) => session
                .vault
                .create_item(&payload)
                .map(|item_id| (item_id, false)),
            (AppState::Unlocked(session), EditorMode::Edit(item_id)) => session
                .vault
                .update_item(item_id, &payload)
                .map(|()| (item_id, true)),
            _ => return,
        };
        match result {
            Ok((item_id, was_restore)) => {
                if mode == EditorMode::Create
                    && let Some(local_icon) = pending_local_icon
                {
                    self.migrate_local_icon_to_item(&local_icon, item_id);
                }
                if let Some(session) = self.session_mut() {
                    session.list.upsert(item_id, payload);
                    session.list.selected = Some(item_id);
                    session.item_editor = None;
                }
                if was_restore {
                    self.refresh_deleted();
                }
                self.editor_blur_subscriptions.clear();
                window.close_sheet(cx);
            }
            Err(_) => {
                if let Some(editor) = self.item_editor_mut() {
                    editor.save_error = Some("Could not save item. Try again.".into());
                }
            }
        }
        cx.notify();
    }

    /// Re-keys a just-saved Create editor's local image from its throwaway
    /// editor key to the new item's real key, so the list/detail resolvers
    /// (keyed by item, not by editor opening) can find it. The bytes are
    /// content-addressed, so this is a cheap local copy, never a re-fetch or
    /// re-upload; the stale editor-keyed entry is dropped from the map since
    /// nothing will ever look it up again.
    fn migrate_local_icon_to_item(
        &mut self,
        local_icon: &crate::icons::LocalIconRef,
        item_id: ItemId,
    ) {
        let new_key = crate::icons::item_key(item_id);
        if local_icon.item_key == new_key {
            self.record_local_icon_selection(local_icon.clone());
            return;
        }
        let data_dir = self.data_dir.clone();
        if let Ok(migrated) =
            crate::icons::cache_local_icon_from_path(&data_dir, &new_key, &local_icon.cache_path)
        {
            self.local_icon_selections.remove(&local_icon.item_key);
            self.record_local_icon_selection(migrated);
        }
    }

    /// Re-read the vault's tombstoned items into list state. Called after any
    /// delete or restore — cheaper to reason about than incremental
    /// bookkeeping, and it can never drift from the projection.
    pub(crate) fn refresh_deleted(&mut self) {
        let AppState::Unlocked(session) = &mut self.state else {
            return;
        };
        if let Ok(deleted) = session.vault.list_deleted_items() {
            session.list.deleted = deleted;
        }
    }

    pub(crate) fn delete_item(
        &mut self,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match &mut self.state {
            AppState::Unlocked(session) => session.vault.delete_item(item_id),
            _ => return,
        };
        match result {
            Ok(()) => {
                if let Some(session) = self.session_mut() {
                    session.list.remove(item_id);
                    session.list.selected = None;
                    session.item_editor = None;
                }
                self.refresh_deleted();
                window.close_sheet(cx);
            }
            Err(_) => {
                if let Some(editor) = self.item_editor_mut() {
                    editor.save_error = Some("Could not delete item. Try again.".into());
                }
            }
        }
        cx.notify();
    }

    /// Save a copy of `item_id` as a new item ("Title (copy)"), selecting the
    /// copy. Reuses the same `Vault::create_item` path a normal save does —
    /// no separate duplication machinery.
    pub(crate) fn duplicate_item(&mut self, item_id: ItemId, cx: &mut Context<Self>) {
        let Some(mut payload) = (match &self.state {
            AppState::Unlocked(session) => session.vault.get_item(item_id).ok().flatten(),
            _ => None,
        }) else {
            return;
        };
        payload.title = format!("{} (copy)", payload.title);
        payload.created_at = now_millis();
        payload.updated_at = payload.created_at;
        let result = match &mut self.state {
            AppState::Unlocked(session) => session.vault.create_item(&payload),
            _ => return,
        };
        match result {
            Ok(new_item_id) => {
                if let Some(session) = self.session_mut() {
                    session.list.upsert(new_item_id, payload);
                    session.list.selected = Some(new_item_id);
                }
            }
            Err(error) => {
                if let Some(session) = self.session_mut() {
                    session.list.load = crate::vault_list::ListLoadState::Failed(
                        format!("Could not duplicate item: {error}").into(),
                    );
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn open_delete_confirmation(
        &mut self,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let locker = cx.entity().downgrade();
        let title = self
            .session()
            .and_then(|session| session.list.items.iter().find(|(id, _)| *id == item_id))
            .map_or_else(
                || "this item".to_owned(),
                |(_, payload)| payload.title.clone(),
            );
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            let locker_for_ok = locker.clone();
            dialog
                .title("Delete item?")
                .child(format!(
                    "Delete \"{title}\"? This item will be moved to Trash. You can restore it from there."
                ))
                .confirm()
                .on_ok(move |_, window, app| {
                    let _ = locker_for_ok
                        .update(app, |locker, cx| locker.delete_item(item_id, window, cx));
                    true
                })
                .on_cancel(|_, _, _| true)
        });
    }

    /// Render the create/edit/restore form. Returns the Sheet title alongside
    /// the body since both are derived from the same editor state snapshot.
    pub(crate) fn render_item_editor(
        &mut self,
        locker: Entity<Nox>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (SharedString, AnyElement) {
        let theme = Theme::current(cx);
        let component_theme = cx.theme();
        let border = component_theme.border;
        let foreground = component_theme.foreground;
        let muted_foreground = component_theme.muted_foreground;
        let danger = component_theme.danger;

        let Some(editor) = self.item_editor() else {
            return (
                "Item".into(),
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(muted_foreground)
                    .child("Select an item or create a new one.")
                    .into_any_element(),
            );
        };
        let mode = editor.mode;
        let item_type = editor.item_type;
        let panel_title = sheet_title(mode, item_type);
        let title = editor.title_input.clone();
        let username = editor.username_input.clone();
        let password = editor.password_input.clone();
        let uris = editor.uri_inputs.clone();
        let notes = editor.notes_input.clone();
        let generated = editor
            .generator
            .generated
            .as_ref()
            .map(|password| String::from_utf8_lossy(password.as_bytes()).into_owned());
        let length_input = editor.generator.length_input.clone();
        let classes = editor.generator.classes;
        let generator_open = editor.generator.open;
        let selected_type = if item_type == ItemType::Login {
            Some(0)
        } else {
            Some(1)
        };
        let save_error = editor.save_error.clone();
        let save_label = "Save";
        let locker_for_generate = locker.clone();
        let locker_for_use = locker.clone();
        let locker_for_type = locker.clone();
        let locker_for_open = locker.clone();
        let class_locker = locker.clone();
        let class_checkbox = move |id: &'static str, label: &'static str, class: CharClasses| {
            Checkbox::new(id)
                .label(label)
                .checked(classes.contains(class))
                .on_click({
                    let locker = class_locker.clone();
                    move |checked, _, app| {
                        locker.update(app, |locker, cx| {
                            locker.set_generator_class(class, *checked, cx)
                        });
                    }
                })
        };
        let generator = Popover::new("password-generator")
            .trigger(
                Button::new("generate-password")
                    .ghost()
                    .xsmall()
                    .label("Generate…"),
            )
            .open(generator_open)
            .on_open_change(move |open, _, app| {
                locker_for_open.update(app, |locker, cx| locker.set_generator_open(*open, cx));
            })
            .content(move |_popover, _window, _cx| {
                let preview = generated.clone().unwrap_or_else(|| "Click Generate".into());
                div()
                    .id("password-generator-panel")
                    .p(px(16.))
                    .w(px(272.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(theme.field_border)
                    .bg(theme.surface)
                    .text_color(theme.text)
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(650.))
                            .child("Password generator"),
                    )
                    .child(Input::new(&length_input))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .font_weight(FontWeight::NORMAL)
                            .child(class_checkbox(
                                "generator-lower",
                                "Lowercase",
                                CharClasses::LOWER,
                            ))
                            .child(class_checkbox(
                                "generator-upper",
                                "Uppercase",
                                CharClasses::UPPER,
                            ))
                            .child(class_checkbox(
                                "generator-digits",
                                "Digits",
                                CharClasses::DIGITS,
                            ))
                            .child(class_checkbox(
                                "generator-symbols",
                                "Symbols",
                                CharClasses::SYMBOLS,
                            )),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(muted_foreground)
                            .truncate()
                            .child(preview),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("regenerate-password")
                                    .outline()
                                    .small()
                                    .label("Generate")
                                    .disabled(classes.is_empty())
                                    .on_click({
                                        let locker = locker_for_generate.clone();
                                        move |_, _, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.generate_editor_password(cx)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("use-generated-password")
                                    .primary()
                                    .small()
                                    .label("Use this password")
                                    .on_click({
                                        let locker = locker_for_use.clone();
                                        move |_, window, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.use_generated_password(window, cx)
                                            });
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            });
        let type_group = RadioGroup::horizontal("item-type")
            .children([
                Radio::new("login-type").label("Login"),
                Radio::new("note-type").label("Secure note"),
            ])
            .selected_index(selected_type)
            .on_click(move |index, _, app| {
                let item_type = if *index == 0 {
                    ItemType::Login
                } else {
                    ItemType::SecureNote
                };
                locker_for_type.update(app, |locker, cx| locker.set_editor_type(item_type, cx));
            });

        let field_label = move |text: &'static str| {
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(foreground)
                .child(text)
        };
        let delete_button = if matches!(mode, EditorMode::Create) {
            div().into_any_element()
        } else {
            Button::new("delete-item")
                .danger()
                .outline()
                .small()
                .label("Delete")
                .on_click({
                    let locker = locker.clone();
                    move |_, window, app| {
                        locker.update(app, |locker, cx| {
                            if let Some(editor) = locker.item_editor()
                                && let EditorMode::Edit(item_id) = editor.mode
                            {
                                locker.open_delete_confirmation(item_id, window, cx);
                            }
                        });
                    }
                })
                .into_any_element()
        };

        let icon_picker = self.render_icon_picker(false, cx);
        let mut content = div()
            .id("item-editor")
            .flex()
            .flex_col()
            .gap(px(18.))
            .py(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(type_group)
                    .child(delete_button),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon_picker)
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(muted_foreground)
                            .child("Tap to change icon"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(field_label("Title"))
                    .child(Input::new(&title)),
            );
        if item_type == ItemType::Login {
            let copy_buttons = match mode {
                EditorMode::Edit(item_id) => div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        Button::new("copy-username")
                            .ghost()
                            .xsmall()
                            .label("Copy username")
                            .on_click({
                                let locker = locker.clone();
                                move |_, window, app| {
                                    locker.update(app, |locker, cx| {
                                        locker.copy_username(item_id, window, cx)
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("copy-password")
                            .ghost()
                            .xsmall()
                            .label("Copy password")
                            .on_click({
                                let locker = locker.clone();
                                move |_, window, app| {
                                    locker.update(app, |locker, cx| {
                                        locker.copy_password(item_id, window, cx)
                                    });
                                }
                            }),
                    ),
                EditorMode::Create => div(),
            };
            content = content
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(field_label("Username"))
                        .child(Input::new(&username)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(field_label("Password"))
                                .child(generator),
                        )
                        .child(Input::new(&password).mask_toggle()),
                )
                .child(copy_buttons)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(field_label("Websites"))
                        .child(website_rows(theme, locker.clone(), uris)),
                );
        }
        content = content.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(field_label("Notes"))
                .child(Textarea::new(&notes)),
        );
        if let Some(error) = save_error {
            content = content.child(div().text_sm().text_color(danger).child(error));
        }
        let content = content
            .child(
                div()
                    .flex()
                    .justify_end()
                    .pt(px(4.))
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Button::new("save-item")
                            .primary()
                            .label(save_label)
                            .on_click({
                                let locker = locker.clone();
                                move |_, window, app| {
                                    locker.update(app, |locker, cx| locker.save_item(window, cx));
                                }
                            }),
                    ),
            )
            .into_any_element();
        (panel_title.into(), content)
    }

    pub(crate) fn render_secure_note_workspace(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(editor) = self.item_editor() else {
            return div().into_any_element();
        };
        let title = editor.title_input.clone();
        let notes = editor.notes_input.clone();
        let note_tag_input = editor.note_tag_input.clone();
        let note_tags = editor.note_tags.clone();
        let notes_value = notes.read(cx).value().to_string();
        let note_has_markdown = secure_note_has_markdown(&notes_value);
        let note_preview_open = editor.markdown_preview_open && note_has_markdown;
        let revealed_copy_blocks = editor.revealed_copy_blocks.clone();
        let character_count = notes_value.chars().count();
        let save_error = editor.save_error.clone();
        let editing = matches!(editor.mode, EditorMode::Edit(_));
        let workspace_title = if editing {
            "Edit note"
        } else {
            "Create secure note"
        };
        let workspace_subtitle = if editing {
            "Update this encrypted note"
        } else {
            "Add an encrypted note to your vault"
        };
        let save_label = if editing { "Save changes" } else { "Save note" };
        let save_hint = if editing {
            "⌘ ↵  Save changes"
        } else {
            "⌘ ↵  Save note"
        };
        let saved_hint = if editing {
            "Encrypted locally · saved in your vault"
        } else {
            "Encrypted locally · not saved yet"
        };
        let back_label =
            if self.session().map(|session| session.active_view) == Some(ActiveView::AllItems) {
                "Back to all items"
            } else {
                "Back to secure notes"
            };
        let locker = cx.entity();

        let cancel_locker = locker.clone();
        let cancel = Button::new("cancel-secure-note")
            .h(px(38.))
            .px(px(14.))
            .rounded(px(8.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.field_border)
            .on_click(move |_, window, app| {
                cancel_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight(500.))
                    .text_color(theme.text_soft)
                    .child("Cancel"),
            );
        let save_locker = locker.clone();
        let save = Button::new("save-secure-note")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .on_click(move |_, window, app| {
                save_locker.update(app, |locker, cx| locker.save_item(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        Icon::empty()
                            .path("icons/check.svg")
                            .size(px(14.))
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight(600.))
                            .text_color(theme.canvas)
                            .child(save_label),
                    ),
            );
        let save = crate::app::animated_auth_button(
            "save-secure-note",
            save,
            self.auth_hovered.get("save-secure-note").copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_press,
                theme.on_inverse,
            ),
            cx,
        );
        let back_locker = locker.clone();
        let back = Button::new("back-to-secure-notes")
            .ghost()
            .h(px(36.))
            .px(px(11.))
            .rounded(px(7.))
            .on_click(move |_, window, app| {
                back_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Icon::empty()
                            .path("icons/arrow-left.svg")
                            .size(px(14.))
                            .text_color(theme.text_secondary),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.text_soft)
                            .child(back_label),
                    ),
            );
        let field = |label: &'static str,
                     required: bool,
                     helper: Option<&'static str>,
                     body: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(9.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.icon_muted)
                                .child(label),
                        )
                        .when(required, |row| {
                            row.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.text_ghost)
                                    .child("Required"),
                            )
                        })
                        .when_some(helper.filter(|_| !required), |row, helper| {
                            row.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.text_ghost)
                                    .child(helper),
                            )
                        }),
                )
                .child(body)
        };
        let tool = |id: &'static str,
                    icon: &'static str,
                    tooltip: &'static str,
                    format: NoteMarkdownFormat| {
            let format_locker = locker.clone();
            Button::new(id)
                .ghost()
                .size(px(26.))
                .rounded(px(6.))
                .tooltip(tooltip)
                .on_click(move |_, window, app| {
                    format_locker.update(app, |locker, cx| {
                        locker.format_secure_note_markdown(format, window, cx);
                    });
                })
                .child(
                    Icon::empty()
                        .path(icon)
                        .size(px(14.))
                        .text_color(theme.icon_muted),
                )
        };
        let preview_toggle = |active: bool| {
            let preview_locker = locker.clone();
            Button::new("secure-note-markdown-preview-toggle")
                .ghost()
                .size(px(26.))
                .rounded(px(6.))
                .tooltip("Preview")
                .when(active, |button| button.bg(theme.raised))
                .on_click(move |_, _window, app| {
                    preview_locker.update(app, |locker, cx| {
                        locker.toggle_secure_note_markdown_preview(cx);
                    });
                })
                .child(
                    Icon::empty()
                        .path("icons/scan-eye.svg")
                        .size(px(14.))
                        .text_color(if active {
                            theme.text_secondary
                        } else {
                            theme.icon_muted
                        }),
                )
        };
        // Pencil "Note Color Field" (`HFk8V`): five 28x28 circular swatches
        // with a 10px gap; the selected swatch carries a 2px ring and a
        // white check, matching the selected Color Option 1 treatment.
        let note_color = editor.note_color;
        let note_color_locker = locker.clone();
        let color_swatches = crate::icons::NOTE_COLOR_CHOICES
            .into_iter()
            .map(|color| {
                let selected = color == note_color;
                let swatch_locker = note_color_locker.clone();
                div()
                    .id(note_color_element_id(color))
                    .size(px(28.))
                    .rounded(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(crate::icons::note_color_hsla(color))
                    .when(selected, |swatch| {
                        swatch.border_2().border_color(rgb(0xD9ECF8)).child(
                            Icon::empty()
                                .path("icons/check.svg")
                                .size(px(13.))
                                .text_color(crate::icons::NOTE_COLOR_GLYPH),
                        )
                    })
                    .when(!selected, |swatch| {
                        swatch.border_1().border_color(gpui::transparent_black())
                    })
                    .hover(|style| style.opacity(0.9))
                    .on_click(move |_, _window, app| {
                        swatch_locker.update(app, |locker, cx| {
                            locker.choose_note_color(color, cx);
                        });
                    })
            })
            .collect::<Vec<_>>();
        let note_tag_query = note_tag_input.read(cx).value().to_string();
        let fixed_suggestion_tags = self.note_tag_suggestions();
        let filtered_tag_options = if note_tag_query.trim().is_empty() {
            Vec::new()
        } else {
            self.note_tag_suggestions_for_query(&note_tag_query)
        };
        let tag_chips = note_tags
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, tag)| {
                let tag_for_remove = tag.clone();
                let remove_locker = locker.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(12.))
                    .bg(theme.raised)
                    .border_1()
                    .border_color(theme.field_border)
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.text_soft)
                            .child(tag),
                    )
                    .child(
                        Button::new(SharedString::from(format!("note-tag-remove-{index}")))
                            .debug_selector(move || format!("note-tag-remove-{index}"))
                            .ghost()
                            .size(px(22.))
                            .rounded(px(11.))
                            .tooltip("Remove tag")
                            .on_click(move |_, _window, app| {
                                remove_locker.update(app, |locker, cx| {
                                    locker.remove_note_tag(&tag_for_remove, cx);
                                });
                            })
                            .child(
                                Icon::empty()
                                    .path("icons/x.svg")
                                    .size(px(12.))
                                    .text_color(theme.text_ghost),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let suggestion_chips = fixed_suggestion_tags
            .iter()
            .cloned()
            .map(|tag| {
                let tag_for_label = tag.clone();
                let add_locker = locker.clone();
                Button::new(note_tag_suggestion_element_id(&tag))
                    .debug_selector({
                        let selector = note_tag_suggestion_element_id(&tag).to_string();
                        move || selector
                    })
                    .ghost()
                    .h(px(22.))
                    .px(px(8.))
                    .rounded(px(11.))
                    .tooltip(SharedString::from(format!("Add tag {tag}")))
                    .on_click(move |_, _window, app| {
                        add_locker.update(app, |locker, cx| {
                            locker.select_note_tag_suggestion(&tag, cx);
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(
                                Icon::empty()
                                    .path("icons/plus.svg")
                                    .size(px(10.))
                                    .text_color(theme.icon_muted),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme.text_soft)
                                    .child(tag_for_label),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let filtered_option_chips = filtered_tag_options
            .iter()
            .cloned()
            .map(|tag| {
                let tag_for_label = tag.clone();
                let tag_input = note_tag_input.clone();
                let add_locker = locker.clone();
                Button::new(note_tag_filter_option_element_id(&tag))
                    .debug_selector({
                        let selector = note_tag_filter_option_element_id(&tag).to_string();
                        move || selector
                    })
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .color(theme.surface)
                            .hover(theme.row_hover),
                    )
                    .w_full()
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .on_click(move |_, window, app| {
                        add_locker.update(app, |locker, cx| {
                            locker.select_note_tag_suggestion(&tag, cx);
                            tag_input.update(cx, |state, input_cx| {
                                state.set_value("".to_owned(), window, input_cx)
                            });
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme.text_soft)
                                    .child(tag_for_label),
                            )
                            .child(
                                Icon::empty()
                                    .path("icons/check.svg")
                                    .size(px(12.))
                                    .text_color(theme.text_ghost),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let tag_filter_top = if note_tags.is_empty() {
            px(42.)
        } else {
            px(74.)
        };
        let note_settings_card = div()
            .id("note-settings-card")
            .flex()
            .flex_col()
            .p(px(20.))
            .gap(px(14.))
            .rounded(px(9.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .child(
                        div()
                            .size(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.))
                            .bg(theme.raised)
                            .child(
                                Icon::empty()
                                    .path("icons/sliders-horizontal.svg")
                                    .size(px(15.))
                                    .text_color(theme.text_secondary),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child("Note settings"),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.text_ghost)
                                    .child("Customize this secure note"),
                            ),
                    ),
            )
            .child(field(
                "COLOR",
                false,
                None,
                div()
                    .id("note-color-options")
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .children(color_swatches)
                    .into_any_element(),
            ))
            .child(field(
                "TAGS",
                false,
                Some("Free-form"),
                div()
                    .id("note-tags-combobox")
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .on_key_down({
                        let tag_locker = locker.clone();
                        move |event: &gpui::KeyDownEvent, window, app| {
                            if event.keystroke.key.as_str() == "enter" {
                                window.prevent_default();
                                tag_locker.update(app, |locker, cx| {
                                    locker.commit_note_tag_input(window, cx);
                                });
                            }
                        }
                    })
                    .child(
                        div()
                            .id("note-tags-value")
                            .flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .children(tag_chips),
                    )
                    .child(
                        div()
                            .id("note-tags-input-shell")
                            .flex()
                            .flex_1()
                            .w_full()
                            .child(
                                div().id("note-tags-input").flex().flex_1().w_full().child(
                                    Input::new(&note_tag_input)
                                        .min_h(px(34.))
                                        .px(px(10.))
                                        .bg(theme.field)
                                        .border_color(theme.field_border)
                                        .rounded(px(7.)),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .id("note-tags-suggestions")
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().text_size(px(9.)).text_color(theme.text_ghost).child(
                                if fixed_suggestion_tags.is_empty() {
                                    "Create tags freely; press Enter to add one."
                                } else {
                                    "Suggestions — most used or latest"
                                },
                            ))
                            .when(!fixed_suggestion_tags.is_empty(), |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap(px(6.))
                                        .children(suggestion_chips),
                                )
                            }),
                    )
                    .when(!note_tag_query.trim().is_empty(), |this| {
                        this.child(
                            div()
                                .id("note-tags-filter-list")
                                .absolute()
                                .top(tag_filter_top)
                                .left_0()
                                .right_0()
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .p(px(4.))
                                .rounded(px(7.))
                                .bg(theme.surface)
                                .border_1()
                                .border_color(theme.border)
                                .shadow_lg()
                                .when(filtered_option_chips.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .id("note-tags-not-found")
                                            .bg(theme.surface)
                                            .h(px(28.))
                                            .px(px(8.))
                                            .flex()
                                            .items_center()
                                            .text_size(px(10.))
                                            .text_color(theme.text_ghost)
                                            .child("Not found"),
                                    )
                                })
                                .when(!filtered_option_chips.is_empty(), |this| {
                                    this.children(filtered_option_chips)
                                }),
                        )
                    })
                    .into_any_element(),
            ));
        let note_editor = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(320.))
            .rounded(px(7.))
            .bg(theme.field)
            .border_1()
            .border_color(theme.field_border)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(38.))
                    .px(px(10.))
                    .gap(px(4.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.field_border)
                    .child(tool(
                        "note-format-bold",
                        "icons/bold.svg",
                        "Bold",
                        NoteMarkdownFormat::Bold,
                    ))
                    .child(tool(
                        "note-format-italic",
                        "icons/italic.svg",
                        "Italic",
                        NoteMarkdownFormat::Italic,
                    ))
                    .child(tool(
                        "note-format-list",
                        "icons/list.svg",
                        "List",
                        NoteMarkdownFormat::List,
                    ))
                    .child(tool(
                        "note-format-code",
                        "icons/code.svg",
                        "Code",
                        NoteMarkdownFormat::Code,
                    ))
                    .child(tool(
                        "note-format-copy-block",
                        "icons/copy.svg",
                        "Copy block",
                        NoteMarkdownFormat::CopyBlock,
                    ))
                    .child(tool(
                        "note-format-copy-locked-block",
                        "icons/eye-off.svg",
                        "Locked copy block",
                        NoteMarkdownFormat::LockedCopyBlock,
                    ))
                    .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(theme.field_border))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme.text_ghost)
                            .child("Markdown supported"),
                    )
                    .child(div().flex_1())
                    .when(note_has_markdown, |toolbar| {
                        toolbar.child(preview_toggle(note_preview_open))
                    }),
            )
            .child(
                Textarea::new(&notes)
                    .appearance(false)
                    .bordered(false)
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(12.))
                    .text_color(theme.text_soft),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(30.))
                    .px(px(12.))
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme.field_border)
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme.text_subtle)
                            .child("Draft saved locally"),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme.text_secondary)
                            .child(format!("{character_count} characters")),
                    ),
            )
            .into_any_element();
        let outcome = |icon: &'static str, heading: &'static str, detail: &'static str| {
            div()
                .flex()
                .gap(px(10.))
                .child(
                    div()
                        .size(px(28.))
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .bg(theme.raised)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::empty()
                                .path(icon)
                                .size(px(13.))
                                .text_color(theme.text_secondary),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text_soft)
                                .child(heading),
                        )
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(theme.text_subtle)
                                .child(detail),
                        ),
                )
        };
        let error = save_error.map_or_else(
            || div().into_any_element(),
            |message| {
                div()
                    .text_size(px(11.))
                    .text_color(theme.danger)
                    .child(message)
                    .into_any_element()
            },
        );
        let icon_picker = self.render_icon_picker(false, cx);
        let _current_icon = self
            .item_editor()
            .map_or(IconChoice::Default, |editor| editor.icon);
        let preview_spans = |spans: Vec<NotePreviewSpan>, text_color, text_size| {
            div()
                .flex()
                .flex_wrap()
                .gap(px(0.))
                .children(spans.into_iter().map(move |span| {
                    div()
                        .text_size(text_size)
                        .text_color(text_color)
                        .font_family(APP_FONT_FAMILY)
                        .when(span.style == NotePreviewSpanStyle::Bold, |span| {
                            span.font_weight(FontWeight::BOLD)
                        })
                        .when(span.style == NotePreviewSpanStyle::Italic, |span| {
                            span.italic()
                        })
                        .when(span.style == NotePreviewSpanStyle::Code, |span| {
                            span.px(px(4.))
                                .rounded(px(4.))
                                .bg(theme.inset)
                                .text_color(theme.text_secondary)
                        })
                        .child(span.text)
                }))
        };
        let preview_blocks = secure_note_markdown_preview_blocks(&notes_value)
            .into_iter()
            .enumerate()
            .map(|(index, block)| {
                let block_id =
                    SharedString::from(format!("secure-note-markdown-preview-block-{index}"));
                match block.kind {
                    NotePreviewBlockKind::CopyBlock => {
                        let copy_text = block.copy_text.clone().unwrap_or_default();
                        let label = block.label.clone();
                        let locked = block.locked;
                        let revealed = revealed_copy_blocks.contains(&index);
                        let copy_locker = locker.clone();
                        let reveal_locker = locker.clone();
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .when_some(label.clone(), |container, label| {
                                container.child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text_soft)
                                        .child(label),
                                )
                            })
                            .child(
                                div()
                                    .id(block_id)
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .p(px(12.))
                                    .rounded(px(8.))
                                    .bg(theme.inset)
                                    .border_1()
                                    .border_color(theme.field_border)
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .text_size(px(11.))
                                                    .text_color(theme.text_subtle)
                                                    .child(if locked && !revealed {
                                                        div()
                                                            .text_size(px(12.))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .text_color(theme.text_secondary)
                                                            .child("••••••••••••••••")
                                                            .into_any_element()
                                                    } else {
                                                        preview_spans(
                                                            block.spans,
                                                            theme.text_subtle,
                                                            px(11.),
                                                        )
                                                        .into_any_element()
                                                    }),
                                            )
                                            .when(locked, |row| {
                                                row.child(
                                                    Button::new(SharedString::from(format!(
                                                        "secure-note-copy-block-reveal-{index}"
                                                    )))
                                                    .ghost()
                                                    .size(px(26.))
                                                    .rounded(px(6.))
                                                    .tooltip(if revealed { "Hide" } else { "Reveal" })
                                                    .on_click(move |_, _window, app| {
                                                        reveal_locker.update(app, |locker, cx| {
                                                            locker.toggle_secure_note_copy_block_reveal(
                                                                index, cx,
                                                            );
                                                        });
                                                    })
                                                    .child(
                                                        Icon::empty()
                                                            .path(if revealed {
                                                                "icons/scan-eye.svg"
                                                            } else {
                                                                "icons/eye-off.svg"
                                                            })
                                                            .size(px(14.))
                                                            .text_color(theme.icon_muted),
                                                    ),
                                                )
                                            })
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "secure-note-copy-block-copy-{index}"
                                                )))
                                                .ghost()
                                                .h(px(26.))
                                                .px(px(8.))
                                                .rounded(px(6.))
                                                .tooltip("Copy block")
                                                .on_click(move |_, window, app| {
                                                    let copy_text = copy_text.clone();
                                                    copy_locker.update(app, |locker, cx| {
                                                        locker
                                                            .copy_secure_note_block(copy_text, window, cx);
                                                    });
                                                })
                                                .child(
                                                    Icon::empty()
                                                        .path("icons/copy.svg")
                                                        .size(px(12.))
                                                        .text_color(theme.text_secondary),
                                                ),
                                            ),
                                    )
                            )
                            .into_any_element()
                    }
                    NotePreviewBlockKind::Heading => div()
                        .id(block_id)
                        .text_size(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(preview_spans(block.spans, theme.text, px(18.)))
                        .into_any_element(),
                    NotePreviewBlockKind::ListItem => div()
                        .id(block_id)
                        .flex()
                        .gap(px(8.))
                        .text_size(px(11.))
                        .text_color(theme.text_soft)
                        .child(
                            div()
                                .mt(px(7.))
                                .size(px(4.))
                                .rounded_full()
                                .bg(theme.text_secondary),
                        )
                        .child(preview_spans(block.spans, theme.text_soft, px(11.)))
                        .into_any_element(),
                    NotePreviewBlockKind::Code => div()
                        .id(block_id)
                        .p(px(10.))
                        .rounded(px(7.))
                        .bg(theme.inset)
                        .border_1()
                        .border_color(theme.field_border)
                        .text_size(px(10.))
                        .text_color(theme.text_secondary)
                        .child(preview_spans(block.spans, theme.text_secondary, px(10.)))
                        .into_any_element(),
                    NotePreviewBlockKind::Paragraph => div()
                        .id(block_id)
                        .text_size(px(11.))
                        .line_height(px(18.))
                        .text_color(theme.text_subtle)
                        .child(preview_spans(block.spans, theme.text_subtle, px(11.)))
                        .into_any_element(),
                }
            })
            .collect::<Vec<_>>();
        let note_preview_panel = div()
            .id("secure-note-markdown-preview-panel")
            .flex()
            .flex_col()
            .h_full()
            .p(px(20.))
            .gap(px(16.))
            .rounded(px(9.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .occlude()
            .with_animation(
                "secure-note-markdown-preview-enter",
                Animation::new(NOTE_PREVIEW_TRANSITION_DURATION).with_easing(ease_out_quint()),
                |panel, delta| panel.opacity(delta).left(px((1. - delta) * 18.)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .child(
                                div()
                                    .size(px(30.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(7.))
                                    .bg(theme.raised)
                                    .child(
                                        Icon::empty()
                                            .path("icons/scan-eye.svg")
                                            .size(px(15.))
                                            .text_color(theme.text_secondary),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child("Markdown preview"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(theme.text_ghost)
                                            .child("Rendered from the encrypted note body"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .bg(theme.raised)
                            .text_size(px(8.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_secondary)
                            .child("PREVIEW"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(preview_blocks),
            )
            .into_any_element();
        let note_cards_panel = div()
            .id("secure-note-helper-cards")
            .flex()
            .flex_col()
            .h_full()
            .gap(px(14.))
            .occlude()
            .with_animation(
                "secure-note-helper-cards-enter",
                Animation::new(NOTE_PREVIEW_TRANSITION_DURATION).with_easing(ease_out_quint()),
                |panel, delta| panel.opacity(delta).left(px((1. - delta) * -14.)),
            )
            .child(note_settings_card)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .p(px(20.))
                    .gap(px(14.))
                    .rounded(px(9.))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(9.))
                                    .child(
                                        div()
                                            .size(px(30.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(7.))
                                            .bg(theme.raised)
                                            .child(
                                                Icon::empty()
                                                    .path("icons/shield-check.svg")
                                                    .size(px(15.))
                                                    .text_color(theme.text_secondary),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.text)
                                                    .child("Note privacy"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(9.))
                                                    .text_color(theme.text_ghost)
                                                    .child("Encrypted the moment you type"),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(24.))
                                    .px(px(8.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(6.))
                                    .bg(theme.raised)
                                    .text_size(px(8.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.success_bright)
                                    .child("ENCRYPTED"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.text_subtle)
                            .child("Notes are encrypted locally before they ever leave this device, and stay unreadable without your master password."),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(9.))
                            .text_size(px(10.))
                            .text_color(theme.text_muted)
                            .child(div().flex().items_center().gap(px(8.)).child(Icon::empty().path("icons/key-round.svg").size(px(13.)).text_color(theme.text_ghost)).child("Wi-Fi passwords and PINs"))
                            .child(div().flex().items_center().gap(px(8.)).child(Icon::empty().path("icons/key-round.svg").size(px(13.)).text_color(theme.text_ghost)).child("Recovery and backup codes"))
                            .child(div().flex().items_center().gap(px(8.)).child(Icon::empty().path("icons/shield-check.svg").size(px(13.)).text_color(theme.text_ghost)).child("Security question answers")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .p(px(20.))
                    .gap(px(13.))
                    .rounded(px(9.))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("After saving"),
                    )
                    .child(outcome("icons/copy-plus.svg", "Quick copy", "The note content becomes a quick copy target."))
                    .child(outcome("icons/search.svg", "Full-text search", "Find this note instantly across your vault."))
                    .child(outcome("icons/refresh-cw.svg", "Sync securely", "The encrypted note syncs with your vault devices.")),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(70.))
                    .px(px(16.))
                    .rounded(px(9.))
                    .bg(theme.inset)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text_soft)
                                    .child("Keyboard friendly"),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.text_ghost)
                                    .child("Tab between fields · Esc to cancel"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_secondary)
                            .child("⌘ ↵"),
                    ),
            )
            .into_any_element();

        rsx! {
            <div id="secure-note-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={theme.canvas}>
                <div flex items_center justify_between h={px(88.)} px={px(32.)} flex_shrink_0 border_b_1 borderColor={theme.border}>
                    <div flex flex_col gap={px(3.)}>
                        <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{workspace_title}</div>
                        <div text_xs textColor={theme.text_muted}>{workspace_subtitle}</div>
                    </div>
                    <div flex items_center gap={px(10.)}>{cancel}{save}</div>
                </div>
                <div flex flex_col flex_1 min_h={px(0.)} p={px(28.)} pt={px(20.)} gap={px(16.)} overflow_y_scroll>
                    <div flex items_center justify_between h={px(40.)} flex_shrink_0>
                        {back}
                        <div flex items_center gap={px(8.)}>
                            {Icon::empty().path("icons/shield-check.svg").size(px(14.)).text_color(theme.text_subtle)}
                            <div text_size={px(9.)} textColor={theme.icon_muted}>{saved_hint}</div>
                        </div>
                    </div>
                    <div flex flex_1 min_h={px(0.)} gap={px(16.)}>
                        <div flex flex_col w={px(760.)} flex_shrink_0 h_full p={px(24.)} gap={px(14.)} rounded={px(9.)} bg={theme.surface} border_1 borderColor={theme.border}>
                            <div flex items_center justify_between h={px(44.)} flex_shrink_0>
                                <div flex flex_col gap={px(4.)}>
                                    <div text_size={px(14.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Note details"}</div>
                                    <div text_size={px(10.)} textColor={theme.text_subtle}>{"Store sensitive text securely, end-to-end encrypted."}</div>
                                </div>
                                <div flex items_center gap={px(8.)}>
                                <div flex items_center gap={px(8.)}>
                                    {icon_picker}
                                </div>
                                </div>
                            </div>
                            {field("TITLE", true, None, Input::new(&title).min_h(px(42.)).px(px(11.)).bg(theme.field).border_color(theme.field_border).rounded(px(7.)).prefix(Icon::empty().path("icons/notebook-pen.svg").size(px(14.)).text_color(theme.icon_muted)).into_any_element())}
                            {field("CONTENT", true, None, note_editor).flex_1().min_h(px(0.))}
                            {error}
                            <div flex items_center h={px(42.)} px={px(11.)} rounded={px(7.)} bg={theme.inset} border_1 borderColor={theme.border}>
                                {Icon::empty().path("icons/lock-keyhole.svg").size(px(13.)).text_color(theme.success)}
                                <div ml={px(8.)} text_size={px(9.)} textColor={theme.text_subtle}>{"Encrypted before it leaves this device"}</div>
                                <div ml_auto text_size={px(9.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_secondary}>{save_hint}</div>
                            </div>
                        </div>
                        <div flex flex_col flex_1 min_w={px(0.)} h_full gap={px(14.)}>
                            {if note_preview_open { note_preview_panel } else { note_cards_panel }}
                        </div>
                    </div>
                </div>
            </div>
        }
        .into_any_element()
    }

    /// The full-page "Create login" workspace (Pencil "Nox — Create Login").
    /// Reuses the same `ItemEditorState`/generator/save machinery as the
    /// Sheet-based editor — only the layout and chrome are new.
    ///
    /// Deliberate omissions, since nothing in the data model backs them:
    /// Folder and Tags fields (no categorization/tagging concept at all —
    /// unlike Cards/IDs elsewhere, there's no reasonable "real 0" to show),
    /// "Add to favorites" (no favorite flag), and "Not found in known
    /// breaches" (would require sending password data to a network service —
    /// not something to wire in silently for a security tool).
    pub(crate) fn render_login_workspace(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(editor) = self.item_editor() else {
            return div().into_any_element();
        };
        let title = editor.title_input.clone();
        let username = editor.username_input.clone();
        let password = editor.password_input.clone();
        let uris = editor.uri_inputs.clone();
        let notes = editor.notes_input.clone();
        let password_value = password.read(cx).value().to_string();
        let generated = editor
            .generator
            .generated
            .as_ref()
            .map(|password| String::from_utf8_lossy(password.as_bytes()).into_owned());
        let length_input = editor.generator.length_input.clone();
        let classes = editor.generator.classes;
        let generator_open = editor.generator.open;
        let save_error = editor.save_error.clone();
        let edit_item_id = match editor.mode {
            EditorMode::Edit(item_id) => Some(item_id),
            EditorMode::Create => None,
        };
        let workspace_title = if edit_item_id.is_some() {
            "Edit login"
        } else {
            "Create login"
        };
        let workspace_subtitle = if edit_item_id.is_some() {
            "Update this secure account in your vault"
        } else {
            "Add a secure account to your vault"
        };
        let save_label = if edit_item_id.is_some() {
            "Save changes"
        } else {
            "Save login"
        };

        let items = self
            .session()
            .map_or(Vec::new(), |session| session.list.items.clone());
        let strength = password_strength_score(&password_value);
        let has_min_length = password_value.chars().count() >= 14;
        let is_reused = password_is_reused(&password_value, &items);
        let has_password = !password_value.is_empty();

        let locker = cx.entity();

        let back_label =
            if self.session().map(|session| session.active_view) == Some(ActiveView::AllItems) {
                "Back to all items"
            } else {
                "Back to logins"
            };
        let back_locker = locker.clone();
        let back_link = Button::new("back-to-logins")
            .ghost()
            .h(px(32.))
            .px(px(4.))
            .on_click(move |_, window, app| {
                back_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .text_color(theme.text_soft)
                    .child("‹")
                    .child(back_label),
            );

        let cancel_locker = locker.clone();
        let cancel_button = Button::new("cancel-create-login")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .bg(theme.surface)
            .on_click(move |_, window, app| {
                cancel_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight(500.))
                    .text_color(theme.text_soft)
                    .child("Cancel"),
            );

        let save_locker = locker.clone();
        let save_button_base = Button::new("save-create-login")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .on_click(move |_, window, app| {
                save_locker.update(app, |locker, cx| locker.save_item(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/check.svg")
                            .size(px(14.))
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.canvas)
                            .child(save_label),
                    ),
            );
        let save_button = crate::app::animated_auth_button(
            "save-create-login",
            save_button_base,
            self.auth_hovered.get("save-create-login").copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_press,
                theme.on_inverse,
            ),
            cx,
        );

        let delete_button = if let Some(item_id) = edit_item_id {
            let delete_locker = locker.clone();
            Button::new("delete-login")
                .danger()
                .outline()
                .small()
                .label("Delete")
                .on_click(move |_, window, app| {
                    delete_locker.update(app, |locker, cx| {
                        locker.open_delete_confirmation(item_id, window, cx);
                    });
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let field = |label: &'static str,
                     required: bool,
                     helper: Option<&'static str>,
                     body: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(7.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight(600.))
                                .text_color(theme.icon_muted)
                                .child(label),
                        )
                        .child(if required {
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_ghost)
                                .child("Required")
                                .into_any_element()
                        } else if let Some(helper) = helper {
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_ghost)
                                .child(helper)
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
                .child(body)
                .into_any_element()
        };
        let dark_input_style = |input: Input| {
            input
                // `min_h`, not `h`: `Input::h` is an inherent method that only
                // reaches the element for multi-line inputs, so a single-line
                // field silently keeps gpui-component's 32px `input_h(size)`.
                // The `Styled` height lands in `refine_style`, which wins.
                .min_h(px(42.))
                .bg(theme.field)
                .border_color(theme.field)
                .rounded(px(8.))
        };

        let generate_open_locker = locker.clone();
        let generate_use_locker = locker.clone();
        let generate_run_locker = locker.clone();
        let generate_class_locker = locker.clone();
        let generate_label = if password.read(cx).value().is_empty() {
            "Generate"
        } else {
            "Regenerate"
        };
        let class_switch = move |id: &'static str,
                                 label: &'static str,
                                 sample: &'static str,
                                 class: CharClasses| {
            let locker = generate_class_locker.clone();
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(20.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight(550.))
                                .text_color(theme.text_soft)
                                .child(label),
                        )
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(theme.text_ghost)
                                .child(sample),
                        ),
                )
                .child(
                    Switch::new(id)
                        .checked(classes.contains(class))
                        .color(theme.border_strong)
                        .on_click(move |checked, _, app| {
                            locker.update(app, |locker, cx| {
                                locker.set_generator_class(class, *checked, cx)
                            });
                        }),
                )
        };
        let generate_trigger = Button::new("generate-password-trigger")
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(theme.raised)
                    .hover(theme.row_hover),
            )
            .border_color(theme.smart_field_border)
            .h(px(28.))
            .px(px(8.))
            .rounded(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/refresh-cw.svg")
                            .size(px(12.))
                            .text_color(theme.text_secondary),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .font_weight(FontWeight(600.))
                            .text_color(theme.text_secondary)
                            .child(generate_label),
                    ),
            );
        let generator_popover = Popover::new("create-login-password-generator")
            // The panel below already paints the card. Without this the
            // component wraps it in a second bordered surface.
            .appearance(false)
            .trigger(generate_trigger)
            .open(generator_open)
            .on_open_change(move |open, _, app| {
                generate_open_locker.update(app, |locker, cx| locker.set_generator_open(*open, cx));
            })
            .content(move |_popover, _window, _cx| {
                div()
                    .id("password-generator-panel")
                    .p(px(20.))
                    .w(px(320.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .rounded(px(14.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .text_color(theme.text)
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight(650.))
                            .child("Password generator"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(8.))
                                    .font_weight(FontWeight(700.))
                                    .text_color(theme.icon_muted)
                                    .child("LENGTH"),
                            )
                            .child(
                                Input::new(&length_input)
                                    .aria_label("Password length")
                                    .min_h(px(40.))
                                    .px(px(14.))
                                    .bg(theme.field)
                                    .border_color(theme.field_border)
                                    .rounded(px(8.)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .font_weight(FontWeight::NORMAL)
                            .child(class_switch(
                                "cl-generator-lower",
                                "Lowercase",
                                "a–z",
                                CharClasses::LOWER,
                            ))
                            .child(class_switch(
                                "cl-generator-upper",
                                "Uppercase",
                                "A–Z",
                                CharClasses::UPPER,
                            ))
                            .child(class_switch(
                                "cl-generator-digits",
                                "Digits",
                                "0–9",
                                CharClasses::DIGITS,
                            ))
                            .child(class_switch(
                                "cl-generator-symbols",
                                "Symbols",
                                "!@#$",
                                CharClasses::SYMBOLS,
                            )),
                    )
                    // The hint only stands in until there is something to show:
                    // a generator whose result is invisible until you commit it
                    // is asking you to accept a password sight unseen.
                    .child(match generated.clone() {
                        Some(value) => div()
                            .id("generated-password-preview")
                            .px(px(12.))
                            .py(px(10.))
                            .rounded(px(8.))
                            .bg(theme.inset)
                            .border_1()
                            .border_color(theme.field_border)
                            .text_size(px(11.))
                            .font_weight(FontWeight(550.))
                            .text_color(theme.text_soft)
                            .child(SharedString::from(value))
                            .into_any_element(),
                        None => div()
                            .text_size(px(9.5))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.text_ghost)
                            .child("Click Generate to create a new password")
                            .into_any_element(),
                    })
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("cl-regenerate-password")
                                    .outline()
                                    .h(px(36.))
                                    .flex_1()
                                    .border_color(theme.field_border)
                                    .text_color(theme.text_soft)
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .font_weight(FontWeight(600.))
                                            .child("Generate"),
                                    )
                                    .disabled(classes.is_empty())
                                    .on_click({
                                        let locker = generate_run_locker.clone();
                                        move |_, _, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.generate_editor_password(cx)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("cl-use-generated-password")
                                    .primary()
                                    .h(px(36.))
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .font_weight(FontWeight(650.))
                                            .child("Use this password"),
                                    )
                                    .disabled(generated.is_none())
                                    .on_click({
                                        let locker = generate_use_locker.clone();
                                        move |_, window, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.use_generated_password(window, cx)
                                            });
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            });

        // Live segments: neutral fill count, not a red/green judgment — matches
        // the design's understated language rather than an alarming meter.
        let strength_bars = div().flex().gap(px(6.)).children((0..4).map(|index| {
            div()
                .flex_1()
                .h(px(5.))
                .rounded(px(3.))
                .bg(if index < strength {
                    theme.border_strong
                } else {
                    theme.item_icon
                })
        }));
        let requirement = |met: bool, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(if met {
                    gpui_component::Icon::empty()
                        .path("icons/check.svg")
                        .size(px(12.))
                        .text_color(theme.text_secondary)
                        .into_any_element()
                } else {
                    div()
                        .size(px(12.))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.text_ghost)
                        .into_any_element()
                })
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(if met {
                            theme.text_secondary
                        } else {
                            theme.text_muted
                        })
                        .child(label),
                )
        };
        let after_saving_row =
            |icon_path: &'static str, title: &'static str, description: &'static str| {
                div()
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(28.))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .bg(theme.raised)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                gpui_component::Icon::empty()
                                    .path(icon_path)
                                    .size(px(13.))
                                    .text_color(theme.text_secondary),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(theme.text_soft)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.text_subtle)
                                    .child(description),
                            ),
                    )
            };

        let error =
            save_error.map(|message| div().text_sm().text_color(theme.danger).child(message));
        let icon_picker = self.render_icon_picker(true, cx);
        // `ztKOA` vs `VQgVd`: the edit badge and the "fetched automatically"
        // helper belong to the resting state. Once a fetch fails, the badge
        // gives way to an upload button that sits beside the box, because
        // there is nothing left to fetch automatically.
        let favicon_failed = self.favicon_fetch_status() == FaviconFetchStatus::Failed;
        let upload_locker = locker.clone();
        let _current_icon = self
            .item_editor()
            .map_or(IconChoice::Default, |editor| editor.icon);
        let login_icon_field = field(
            "ICON",
            false,
            (!favicon_failed).then_some("Fetched automatically from website"),
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(
                            div()
                                .id("login-avatar-row")
                                .size(px(64.))
                                .relative()
                                .child(icon_picker)
                                .when(!favicon_failed, |this| {
                                    this.child(
                                        div()
                                            .absolute()
                                            .right(px(0.))
                                            .bottom(px(0.))
                                            .size(px(20.))
                                            .rounded_full()
                                            .border_2()
                                            .border_color(theme.canvas)
                                            .bg(theme.raised)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                gpui_component::Icon::empty()
                                                    .path("icons/image-plus.svg")
                                                    .size(px(10.))
                                                    .text_color(theme.text_secondary),
                                            ),
                                    )
                                }),
                        )
                        .when(favicon_failed, |this| {
                            this.child(
                                Button::new("login-avatar-upload")
                                    .h(px(34.))
                                    .px(px(12.))
                                    .rounded(px(8.))
                                    .bg(theme.field)
                                    .border_1()
                                    .border_color(theme.field_border)
                                    .on_click(move |_, window, app| {
                                        upload_locker.update(app, |locker, cx| {
                                            locker.choose_uploaded_item_icon(window, cx);
                                        });
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(7.))
                                            .child(
                                                gpui_component::Icon::empty()
                                                    .path("icons/upload.svg")
                                                    .size(px(13.))
                                                    .text_color(theme.text_secondary),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.))
                                                    .font_weight(FontWeight(600.))
                                                    .text_color(theme.text_soft)
                                                    .child("Upload from device"),
                                            ),
                                    ),
                            )
                        }),
                )
                .into_any_element(),
        );

        let form_card = div()
            .id("create-login-form")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .rounded(px(10.))
            .bg(theme.surface)
            .overflow_y_scroll()
            .p(px(24.))
            .gap(px(24.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child("Account details"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.text_subtle)
                                    .child("Store credentials and sign-in information securely."),
                            ),
                    )
                    .child(
                        div().flex().items_center().gap(px(8.)).child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(26.))
                                .px(px(10.))
                                .rounded(px(6.))
                                .bg(theme.raised)
                                .child(
                                    gpui_component::Icon::empty()
                                        .path("icons/key-square.svg")
                                        .size(px(12.))
                                        .text_color(theme.text_secondary),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight(700.))
                                        .text_color(theme.text_secondary)
                                        .child("LOGIN"),
                                ),
                        ),
                    ),
            )
            .child(login_icon_field)
            .child(field(
                "NAME",
                true,
                None,
                dark_input_style(
                    Input::new(&title).aria_label("Login name").prefix(
                        gpui_component::Icon::empty()
                            .path("icons/key-round.svg")
                            .size(px(14.))
                            .text_color(theme.icon_muted),
                    ),
                )
                .into_any_element(),
            ))
            .child(field(
                "USERNAME",
                false,
                Some("Click row later to copy"),
                dark_input_style(Input::new(&username).aria_label("Username")).into_any_element(),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(8.))
                                    .font_weight(FontWeight(700.))
                                    .text_color(theme.icon_muted)
                                    .child("PASSWORD"),
                            )
                            .child(
                                div()
                                    .text_size(px(8.))
                                    .text_color(theme.success)
                                    .child("Generated locally"),
                            ),
                    )
                    .child(
                        dark_input_style(
                            Input::new(&password)
                                .mask_toggle()
                                .aria_label("Password")
                                .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/lock.svg")
                                        .size(px(14.))
                                        .text_color(theme.icon_muted),
                                ),
                        )
                        .min_h(px(44.))
                        .border_color(theme.smart_field_border)
                        .rounded(px(7.)),
                    )
                    .child(
                        div()
                            .id("generate-password-trigger-row")
                            .flex()
                            .justify_end()
                            .child(generator_popover),
                    ),
            )
            .child(field(
                "WEBSITES",
                false,
                Some("Add sign-in or app URLs"),
                website_rows(theme, locker.clone(), uris),
            ))
            .child(field(
                "NOTES",
                false,
                None,
                Textarea::new(&notes)
                    .bg(theme.field)
                    .border_color(theme.field)
                    .rounded(px(8.))
                    .into_any_element(),
            ))
            .children(error)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(38.))
                    .px(px(14.))
                    .rounded(px(8.))
                    .bg(theme.inset)
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_subtle)
                            .child("Encrypted before it leaves this device"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_secondary)
                            .child("Ctrl + Enter to save"),
                    ),
            )
            .into_any_element();

        let guidance_card = div()
            .flex()
            .flex_col()
            .w(px(384.))
            .flex_shrink_0()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(20.))
                    .rounded(px(10.))
                    .bg(theme.surface)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .size(px(30.))
                                            .rounded(px(8.))
                                            .bg(theme.raised)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                gpui_component::Icon::empty()
                                                    .path("icons/shield-check.svg")
                                                    .size(px(15.))
                                                    .text_color(theme.text_secondary),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.text)
                                                    .child("Password health"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(theme.text_ghost)
                                                    .child("Updates as you type"),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(22.))
                                    .px(px(9.))
                                    .rounded(px(6.))
                                    .bg(theme.raised)
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .font_weight(FontWeight(700.))
                                            .text_color(theme.text_subtle)
                                            .child(if has_password { "LIVE" } else { "PENDING" }),
                                    ),
                            ),
                    )
                    .child(strength_bars)
                    .child(if has_password {
                        div().into_any_element()
                    } else {
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_subtle)
                            .child("Enter a password or generate one to check its strength.")
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(requirement(
                                has_password && has_min_length,
                                "At least 14 characters",
                            ))
                            .child(requirement(
                                has_password && !is_reused,
                                "Unique and not reused",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .size(px(12.))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(theme.text_ghost),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(theme.text_muted)
                                            .child("Breach check unavailable offline"),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(20.))
                    .rounded(px(10.))
                    .bg(theme.surface)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("After saving"),
                    )
                    .child(after_saving_row(
                        "icons/ellipsis-vertical.svg",
                        "One-click copy",
                        "Username and password rows become quick copy targets.",
                    ))
                    .child(after_saving_row(
                        "icons/external-link.svg",
                        "Open the website",
                        "The website row opens directly in your browser.",
                    ))
                    .child(after_saving_row(
                        "icons/cloud-check.svg",
                        "Stored encrypted",
                        "Ready to sync the moment you pair another device.",
                    )),
            )
            .into_any_element();

        div()
            .id("create-login-workspace")
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(theme.canvas)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.cancel_item_editor(window, cx),
                    "enter"
                        if event.keystroke.modifiers.control
                            || event.keystroke.modifiers.platform =>
                    {
                        this.save_item(window, cx)
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(88.))
                    .px(px(32.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(workspace_title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.text_muted)
                                    .child(workspace_subtitle),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(cancel_button)
                            .child(delete_button)
                            .child(save_button),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(28.))
                    .pt(px(20.))
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(back_link)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        gpui_component::Icon::empty()
                                            .path("icons/lock-keyhole.svg")
                                            .size(px(14.))
                                            .text_color(theme.text_subtle),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(theme.icon_muted)
                                            .child("Encrypted locally · not saved yet"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h(px(0.))
                            .gap(px(16.))
                            .child(form_card)
                            .child(guidance_card),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_login(password: &str) -> ItemPayload {
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type: ItemType::Login,
            title: "Existing".into(),
            username: "alex".into(),
            password: password.into(),
            uris: vec![],
            notes: String::new(),
            created_at: 1,
            updated_at: 1,
            icon: IconChoice::Default,
            note_color: NoteColor::Blue,
            note_tags: vec![],
            favorite: false,
        }
    }

    #[test]
    fn strength_score_rewards_length_and_variety_and_treats_empty_as_zero() {
        assert_eq!(password_strength_score(""), 0);
        assert_eq!(password_strength_score("short"), 1);
        assert_eq!(password_strength_score("longerpassword"), 3);
        assert_eq!(password_strength_score("Lo7ng3rP@ssword!"), 4);
    }

    #[test]
    fn reuse_check_only_matches_saved_login_passwords() {
        let items = vec![
            (ItemId::new(), saved_login("shared-secret")),
            (ItemId::new(), {
                let mut note = saved_login("note-body");
                note.item_type = ItemType::SecureNote;
                note
            }),
        ];
        assert!(password_is_reused("shared-secret", &items));
        assert!(!password_is_reused("note-body", &items));
        assert!(!password_is_reused("unused-password", &items));
        // Empty means "not set yet", not a reuse collision with other unset fields.
        assert!(!password_is_reused("", &items));
    }

    #[test]
    fn create_login_generate_trigger_matches_pencil_smart_field() {
        let source = include_str!("item_editor.rs");
        assert!(source.contains("\"icons/refresh-cw.svg\""));
        assert!(source.contains(
            ".color(theme.raised)\n                    .hover(theme.row_hover),\n            )\n            .border_color(theme.smart_field_border)"
        ));
    }

    #[test]
    fn password_generator_popovers_use_nox_panel_chrome() {
        let source = include_str!("item_editor.rs");
        assert_eq!(
            source.matches(".id(\"password-generator-panel\")").count(),
            2
        );
        assert!(source.matches(".bg(theme.surface)").count() >= 2);
        assert!(source.matches(".border_color(theme.field_border)").count() >= 2);
        assert!(source.contains(".w(px(320.))"));
        assert!(source.contains(".rounded(px(14.))"));
        assert!(source.contains("Switch::new(id)"));
        assert!(source.contains("\"generate-password-trigger-row\""));
        assert!(source.contains("\"generated-password-preview\""));
        assert!(source.contains("Generated locally"));
    }

    /// The secure-note workspace renders one swatch per Pencil "Note Color
    /// Field" choice (`locker.pen` `HFk8V`/`x8fus6`) with the exact tokens, and
    /// the chosen color flows into the encrypted payload and back out on edit.
    #[test]
    fn secure_note_workspace_has_pencil_note_color_swatches() {
        let source = include_str!("item_editor.rs");
        for swatch in [
            "note-color-neutral",
            "note-color-blue",
            "note-color-purple",
            "note-color-orange",
            "note-color-gold",
            "note-color-green",
        ] {
            assert!(
                source.contains(&format!("\"{swatch}\"")),
                "the workspace must render swatch {swatch}"
            );
        }
        assert!(source.contains("Note settings"));
        assert!(source.contains("Customize this secure note"));
        assert!(source.contains("note-color-options"));
        assert!(source.contains("note-tags-combobox"));
        assert!(source.contains("note-tags-input"));
        assert!(source.contains("commit_note_tag_input(window, cx)"));
        assert!(source.contains(".flex_1()"));
        assert!(!source.contains("Button::new(\"note-tags-add\")"));
        assert!(!source.contains(".child(\"Add\")"));
        assert!(source.contains("fn choose_note_color("));
        assert!(source.contains("fn add_note_tag("));
        assert!(source.contains("fn remove_note_tag("));
        assert!(source.contains("note_color: self.note_color"));
        assert!(source.contains("note_tags: nox_core::normalize_note_tags(&self.note_tags)"));
        assert!(source.contains("editor.note_color = payload.note_color"));
        assert!(source.contains("editor.note_tags = payload.note_tags"));

        // The note's own icon chip (top of "Note details") is the only icon
        // preview living inside the editor itself, so clicking a swatch has
        // to visibly retint it live — otherwise picking a color has no
        // visible result until the note is saved and viewed elsewhere.
        assert!(source.contains("let note_tint = (item_type == ItemType::SecureNote"));
        assert!(source.contains(".bg(chip_bg)"));
        // The chip's border rings the icon in its own stroke color, not a
        // separate/default accent.
        assert!(source.contains(".border_color(chip_border)"));

        // The five Pencil hex tokens live with the shared render helpers.
        let icons = include_str!("icons.rs");
        for token in ["0x4A9FD8", "0x9B7BD7", "0xD98B60", "0xD2B45B", "0x58A887"] {
            assert!(
                icons.contains(token),
                "missing Pencil note color token {token}"
            );
        }
        // Stored colors reach the list and detail icon wells.
        let vault_list = include_str!("vault_list.rs");
        assert!(vault_list.contains("note_color_hsla(payload.note_color)"));
        let detail = include_str!("detail.rs");
        assert!(detail.contains("note_color_hsla(payload.note_color)"));
    }

    /// The tags combobox keeps shadcn multiple-combobox semantics: chips carry
    /// a real remove button with a stable per-chip id, saved secure-note tags
    /// render under the input as selectable suggestions, and Enter — never an
    /// "Add" button — commits typed text.
    #[test]
    fn secure_note_markdown_toolbar_formats_selected_text() {
        let bold = apply_note_markdown_format("alpha beta", 6..10, NoteMarkdownFormat::Bold);
        assert_eq!(bold.text, "alpha **beta**");
        assert_eq!(bold.selection, 8..12);

        let italic = apply_note_markdown_format("alpha beta", 6..10, NoteMarkdownFormat::Italic);
        assert_eq!(italic.text, "alpha *beta*");
        assert_eq!(italic.selection, 7..11);

        let code = apply_note_markdown_format("alpha beta", 6..10, NoteMarkdownFormat::Code);
        assert_eq!(code.text, "alpha `beta`");
        assert_eq!(code.selection, 7..11);
    }

    #[test]
    fn secure_note_markdown_toolbar_formats_current_word_and_lines() {
        let bold = apply_note_markdown_format("alpha beta", 8..8, NoteMarkdownFormat::Bold);
        assert_eq!(bold.text, "alpha **beta**");
        assert_eq!(bold.selection, 8..12);

        let list = apply_note_markdown_format("first\nsecond", 0..12, NoteMarkdownFormat::List);
        assert_eq!(list.text, "- first\n- second");
        assert_eq!(list.selection, 2..16);
    }

    #[test]
    fn secure_note_markdown_detection_controls_preview_toggle() {
        assert!(!secure_note_has_markdown("plain private note"));
        assert!(secure_note_has_markdown("# Recovery\n- code one"));
        assert!(secure_note_has_markdown("Store **important** value"));
        assert!(secure_note_has_markdown("Use `ssh-keygen`"));
        assert!(secure_note_has_markdown("Read [docs](https://example.com)"));
    }

    #[test]
    fn secure_note_icon_picker_is_single_combined_note_chip() {
        let source = include_str!("item_editor.rs");
        assert!(source.contains("else if item_type == ItemType::SecureNote"));
        assert!(source.contains(".child(\"NOTE\")"));
        assert!(!source.contains("icons/file-lock.svg\").size(px(12.))"));
    }

    #[test]
    fn secure_note_copy_blocks_parse_as_independent_preview_cards() {
        let blocks = secure_note_markdown_preview_blocks(
            ":::copy API token\nvisible-token\n:::\n:::copy-locked\nsecret-token\n:::",
        );
        assert_eq!(blocks[0].kind, NotePreviewBlockKind::CopyBlock);
        assert_eq!(blocks[0].label.as_deref(), Some("API token"));
        assert!(!blocks[0].locked);
        assert_eq!(blocks[0].copy_text.as_deref(), Some("visible-token"));
        assert_eq!(blocks[0].text(), "visible-token");
        assert_eq!(blocks[1].kind, NotePreviewBlockKind::CopyBlock);
        assert_eq!(blocks[1].label, None);
        assert!(blocks[1].locked);
        assert_eq!(blocks[1].copy_text.as_deref(), Some("secret-token"));
    }

    #[test]
    fn secure_note_copy_block_toolbar_inserts_directive_snippets() {
        let copy = apply_note_markdown_format("token", 0..5, NoteMarkdownFormat::CopyBlock);
        assert_eq!(copy.text, ":::copy\ntoken\n:::");
        assert_eq!(copy.selection, 8..13);

        let locked = apply_note_markdown_format("token", 0..5, NoteMarkdownFormat::LockedCopyBlock);
        assert_eq!(locked.text, ":::copy-locked\ntoken\n:::");
        assert_eq!(locked.selection, 15..20);
    }

    #[test]
    fn secure_note_markdown_preview_renders_basic_markdown() {
        let blocks = secure_note_markdown_preview_blocks("# Title\n- one\nUse `code` and **bold**");
        assert_eq!(blocks[0].kind, NotePreviewBlockKind::Heading);
        assert_eq!(blocks[0].text(), "Title");
        assert_eq!(blocks[1].kind, NotePreviewBlockKind::ListItem);
        assert_eq!(blocks[1].text(), "one");
        assert_eq!(blocks[2].kind, NotePreviewBlockKind::Paragraph);
        assert_eq!(blocks[2].text(), "Use code and bold");
        assert_eq!(blocks[2].spans[1].style, NotePreviewSpanStyle::Code);
        assert_eq!(blocks[2].spans[3].style, NotePreviewSpanStyle::Bold);

        let italic = secure_note_markdown_preview_blocks("*italic*");
        assert_eq!(italic[0].spans[0].style, NotePreviewSpanStyle::Italic);
    }

    #[test]
    fn secure_note_markdown_toolbar_buttons_are_wired_to_editor_state() {
        let source = include_str!("item_editor.rs");
        for action in [
            "NoteMarkdownFormat::Bold",
            "NoteMarkdownFormat::Italic",
            "NoteMarkdownFormat::List",
            "NoteMarkdownFormat::Code",
            "NoteMarkdownFormat::CopyBlock",
            "NoteMarkdownFormat::LockedCopyBlock",
        ] {
            assert!(source.contains(action), "missing toolbar action {action}");
        }
        assert!(source.contains(".tooltip(tooltip)"));
        for tooltip in [
            "Bold",
            "Italic",
            "List",
            "Code",
            "Copy block",
            "Locked copy block",
        ] {
            assert!(source.contains(&format!("\"{tooltip}\",")));
        }
        assert!(source.contains("format_secure_note_markdown"));
        assert!(source.contains("toggle_secure_note_markdown_preview"));
        assert!(source.contains("secure-note-markdown-preview-toggle"));
        assert!(source.contains("icons/scan-eye.svg"));
        assert!(source.contains("secure-note-markdown-preview-panel"));
        assert!(source.contains("secure-note-copy-block-copy-{index}"));
        assert!(source.contains("secure-note-copy-block-reveal-{index}"));
        assert!(source.contains("copy_secure_note_block(copy_text"));
        assert!(source.contains("span.text_size(text_size)"));
        assert!(source.contains("span.font_weight(FontWeight(800.))"));
        assert!(source.contains("span.italic()"));
        assert!(
            source.contains("field(\"CONTENT\", true, None, note_editor).flex_1().min_h(px(0.))")
        );
        assert!(source.contains(".min_h(px(320.))"));
        assert!(source.contains("state.replace_all(edit.text"));
        assert!(source.contains("state.set_selected_range(edit.selection"));
    }

    #[test]
    fn note_tags_combobox_has_selectable_suggestions_and_real_remove_buttons() {
        let source = include_str!("item_editor.rs");
        assert!(source.contains("note-tags-combobox"));
        assert!(source.contains("note-tags-suggestions"));
        assert!(source.contains("note-tags-filter-list"));
        assert!(source.contains("note-tags-not-found"));
        assert!(source.contains(".bg(theme.surface)"));
        assert!(source.contains(".border_color(theme.border)"));
        assert!(source.contains("Not found"));
        assert!(source.contains(".absolute()"));
        assert!(source.contains(".top(tag_filter_top)"));
        // No Add button: Enter commits typed text; chips and suggestions are
        // the only controls.
        assert!(!source.contains("Button::new(\"note-tags-add\")"));
        assert!(!source.contains(".child(\"Add\")"));
        // Every chip's remove affordance is a real button with a unique
        // per-chip id, sized for a pointer — not a tiny clickable div.
        assert!(
            source
                .contains("Button::new(SharedString::from(format!(\"note-tag-remove-{index}\")))")
        );
        assert!(source.contains("remove_note_tag(&tag_for_remove, cx)"));
        // Suggestions are selectable chips fed by saved secure-note tags, not
        // static helper text alone.
        assert!(source.contains("note-tag-suggestion-{sanitized}"));
        assert!(source.contains("note-tag-filter-option-{sanitized}"));
        assert!(source.contains("fn note_tag_suggestions("));
        assert!(source.contains("fn select_note_tag_suggestion("));
        assert!(source.contains("payload.item_type == ItemType::SecureNote"));
    }
}
