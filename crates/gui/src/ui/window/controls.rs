use gpui::{
    AnyElement, App, Context, CursorStyle, Div, Entity, FocusHandle, Focusable, KeyDownEvent,
    Keystroke, MouseButton, MouseDownEvent, ResizeEdge, Window, actions, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, Sizable, WindowExt,
    button::{Button, ButtonVariants as _},
    command::{Command, CommandItem, CommandState},
    kbd::Kbd,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_rsx::rsx;
use std::{cell::Cell, rc::Rc};

use crate::{
    assets::{IconName, icon, logo},
    theme::Theme,
};

// use crate::assets::{IconName};

actions!(window_controls, [OpenCommandPalette]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowCommand {
    Minimize,
    ToggleMaximize,
    NewVault,
    OpenVault,
    LockVault,
    GeneratePassword,
    Close,
}

impl WindowCommand {
    pub const ALL: [Self; 7] = [
        Self::Minimize,
        Self::ToggleMaximize,
        Self::NewVault,
        Self::OpenVault,
        Self::LockVault,
        Self::GeneratePassword,
        Self::Close,
    ];

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Minimize => "Minimize",
            Self::ToggleMaximize => "Maximize / Restore",
            Self::NewVault => "New Vault",
            Self::OpenVault => "Open Vault",
            Self::LockVault => "Lock Vault",
            Self::GeneratePassword => "Generate Password",
            Self::Close => "Close Window",
        }
    }
}

fn command_items() -> impl Iterator<Item = CommandItem> {
    WindowCommand::ALL
        .into_iter()
        .map(|command| CommandItem::new().label(command.label()))
}

type CommandHandler = Rc<dyn Fn(WindowCommand, &mut Window, &mut App)>;

/// Title-bar controls and the transient command palette.
pub struct WindowControls {
    pub(crate) command_search: Entity<CommandState>,
    pub(crate) palette_open: bool,
    pub(crate) prior_focus: Option<FocusHandle>,
    command_handler: CommandHandler,
    /// Whether the command search trigger is shown. It only makes sense once
    /// there's a vault open to search within — kept hidden on the create/unlock
    /// screens rather than shown-but-empty.
    authenticated: bool,
}

impl WindowControls {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let command_search = cx.new(|cx| CommandState::new(window, cx));
        Self {
            command_search,
            palette_open: false,
            prior_focus: None,
            command_handler: Rc::new(|_, _, _| {}),
            authenticated: false,
        }
    }

    /// Show or hide the command search trigger. Called on every render from
    /// the owning `Nox` view so this always reflects `AppState`, with no
    /// separate transition site to keep in sync.
    pub fn set_authenticated(&mut self, authenticated: bool, cx: &mut Context<Self>) {
        if self.authenticated != authenticated {
            self.authenticated = authenticated;
            cx.notify();
        }
    }

    pub fn with_command_handler(
        mut self,
        handler: impl Fn(WindowCommand, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.command_handler = Rc::new(handler);
        self
    }

    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.authenticated {
            return;
        }
        if self.palette_open && window.has_active_dialog(cx) {
            self.command_search.update(cx, |input, cx| {
                input.set_query("", window, cx);
            });
            self.command_search
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
            cx.notify();
            return;
        }
        self.prior_focus = window.focused(cx);
        self.palette_open = true;
        self.command_search.update(cx, |input, cx| {
            input.set_query("", window, cx);
        });
        let command = self.command_search.clone();
        let owner = cx.weak_entity();
        let focus_on_mount = Rc::new(Cell::new(true));
        window.open_dialog(cx, move |dialog, _, _| {
            let command = command.clone();
            let owner = owner.clone();
            let close_owner = owner.clone();
            let focus_on_mount = focus_on_mount.clone();
            dialog
                .close_button(false)
                .p_0()
                .on_close(move |_, _, cx| {
                    let _ = close_owner.update(cx, |controls, cx| {
                        controls.palette_open = false;
                        controls.prior_focus = None;
                        cx.notify();
                    });
                })
                .content(move |content, window, cx| {
                    if focus_on_mount.replace(false) {
                        let command = command.clone();
                        window.defer(cx, move |window, cx| {
                            command.read(cx).focus_handle(cx).focus(window, cx);
                        });
                    }
                    let confirm_owner = owner.clone();
                    content.child(
                        div()
                            .debug_selector(|| "window-command-palette-dialog".to_owned())
                            .child(
                                Command::new(&command)
                                    .bordered(false)
                                    .placeholder("Search commands…")
                                    .items(command_items())
                                    .on_confirm(move |index, window, cx| {
                                        if let Some(command) =
                                            WindowCommand::ALL.get(index.row).copied()
                                        {
                                            let _ = confirm_owner.update(cx, |controls, cx| {
                                                controls.invoke_command(command, window, cx);
                                            });
                                        }
                                    }),
                            ),
                    )
                })
        });
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.palette_open {
            return;
        }
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
            self.palette_open = false;
            self.prior_focus = None;
            return;
        }
        self.palette_open = false;
        if let Some(previous_focus) = self.prior_focus.take() {
            previous_focus.focus(window, cx);
        } else {
            window.focus_next(cx);
        }
    }

    fn invoke_command(
        &mut self,
        command: WindowCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_palette(window, cx);
        (self.command_handler)(command, window, cx);
        cx.notify();
    }

    fn command_callback(
        &self,
        command: WindowCommand,
        cx: &mut Context<Self>,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let controls = cx.entity().downgrade();
        move |_, window, app| {
            let _ = controls.update(app, |controls, cx| {
                controls.invoke_command(command, window, cx);
            });
        }
    }

    fn command_key_callback(
        &self,
        command: WindowCommand,
        cx: &mut Context<Self>,
    ) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
        let controls = cx.entity().downgrade();
        move |event, window, app| {
            if !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                window.prevent_default();
                let _ = controls.update(app, |controls, cx| {
                    controls.invoke_command(command, window, cx);
                });
            }
        }
    }

    fn render_file_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let controls = cx.entity().downgrade();
        let authenticated = self.authenticated;
        rsx! {
            <Button
                base={Button::new("window-file-menu")}
                label={"File"}
                bg={cx.theme().transparent}
                textColor={theme.text_muted}
                border_0
                // Keep title-bar controls out of the vault form's tab order; Ctrl+P
                // provides the keyboard route to every native window command.
                tab_stop={false}
                small
                dropdown_menu={move |menu, _, _| {
                    let new_controls = controls.clone();
                    let open_controls = controls.clone();
                    let lock_controls = controls.clone();
                    let close_controls = controls.clone();
                    menu.item(
                        PopupMenuItem::new("New Vault")
                            .disabled(!authenticated)
                            .on_click(move |_, window, app| {
                                let _ = new_controls.update(app, |controls, cx| {
                                    controls.invoke_command(WindowCommand::NewVault, window, cx);
                                });
                            }),
                    )
                        .item(
                            PopupMenuItem::new("Open Vault")
                                .disabled(!authenticated)
                                .on_click(move |_, window, app| {
                                    let _ = open_controls.update(app, |controls, cx| {
                                        controls.invoke_command(WindowCommand::OpenVault, window, cx);
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Lock Vault")
                                .disabled(!authenticated)
                                .on_click(move |_, window, app| {
                                    let _ = lock_controls.update(app, |controls, cx| {
                                        controls.invoke_command(WindowCommand::LockVault, window, cx);
                                    });
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new("Close Window").on_click(move |_, window, app| {
                                let _ = close_controls.update(app, |controls, cx| {
                                    controls.invoke_command(WindowCommand::Close, window, cx);
                                });
                            }),
                        )
                }}
            />
        }
    }

    fn render_help_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        rsx! {
            <Button
                base={Button::new("window-help-menu")}
                label={"Help"}
                bg={cx.theme().transparent}
                textColor={theme.text_muted}
                border_0
                // Keep title-bar controls out of the vault form's tab order; Ctrl+P
                // provides the keyboard route to every native window command.
                tab_stop={false}
                small
                dropdown_menu={|menu, _, _| {
                    menu.item(PopupMenuItem::new("Keyboard Shortcuts").disabled(true))
                        .item(PopupMenuItem::new("About Nox").disabled(true))
                }}
            />
        }
    }

    fn render_shell(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let open =
            cx.listener(|this, _: &gpui::ClickEvent, window, cx| this.open_palette(window, cx));
        let drag_left = cx.listener(|_, _: &MouseDownEvent, window, _| {
            window.start_window_move();
        });
        let drag_right = cx.listener(|_, _: &MouseDownEvent, window, _| {
            window.start_window_move();
        });
        let lock_button = if self.authenticated {
            rsx! {
                <Button
                    base={Button::new("window-lock-vault")
                        .ghost()
                        .icon(Icon::empty().path("icons/lock-keyhole-open.svg").text_color(theme.text_muted))
                        .tooltip("Lock vault")
                        .on_key_down(self.command_key_callback(WindowCommand::LockVault, cx))}
                    bg={cx.theme().transparent}
                    border_0
                    onClick={self.command_callback(WindowCommand::LockVault, cx)}
                />
            }
            .into_any_element()
        } else {
            div().into_any_element()
        };

        let generate_password_button = if self.authenticated {
            rsx! {
                <Button
                    base={Button::new("window-generate-password")
                        .ghost()
                        .icon(Icon::empty().path("icons/wand-sparkles.svg").text_color(theme.text_muted))
                        .tooltip("Generate password")
                        .on_key_down(self.command_key_callback(WindowCommand::GeneratePassword, cx))}
                    bg={cx.theme().transparent}
                    border_0
                    onClick={self.command_callback(WindowCommand::GeneratePassword, cx)}
                />
            }
            .into_any_element()
        } else {
            div().into_any_element()
        };

        let search_trigger = if self.authenticated {
            rsx! {
                <div items_center justify_center gap={px(8.)}>
                    <Button
                        base={Button::new("window-command-palette-trigger")
                            .debug_selector(|| "window-command-palette-trigger".to_owned())}
                        min_w={px(200.)}
                        onClick={open}
                        bg={theme.canvas}
                        border_color={cx.theme().input}
                        tab_stop={false}
                    >
                        <div flex items_center w_full gap={px(8.)}>
                            {icon(IconName::Search, Some(14.), Some(theme.text_muted))}
                            <div flex_1 text_color={cx.theme().muted_foreground}>
                                {"Search commands…"}
                            </div>
                            <Kbd base={Kbd::new(Keystroke::parse("ctrl-p").unwrap())} />
                        </div>
                    </Button>
                </div>
            }
            .into_any_element()
        } else {
            div().into_any_element()
        };

        rsx! {
            <div
                id="window-controls-shell"
                flex
                items_center
                justify_between
                w_full
                h={px(44.)}
                px={px(12.)}
                gap={px(4.)}
                bg={theme.canvas}
                border_b_1
                borderColor={theme.border}
            >
                <div flex items_center gap={px(4.)}>
                    <div flex items_center gap={px(7.)} mr={px(8.)} textColor={theme.text}>
                        {logo(18., theme.text)}
                    </div>
                    {self.render_file_menu(cx)}
                    {self.render_help_menu(cx)}
                </div>
                <div
                    flex_1
                    h_full
                    debugSelector={|| "window-titlebar-drag-left".to_owned()}
                    onMouseDown={(MouseButton::Left, drag_left)}
                />
                {search_trigger}
                <div
                    flex_1
                    h_full
                    debugSelector={|| "window-titlebar-drag-right".to_owned()}
                    onMouseDown={(MouseButton::Left, drag_right)}
                />
                <div flex items_center gap={px(4.)}>
                    {lock_button}
                    {generate_password_button}
                    <Button
                        base={Button::new("window-minimize")}
                        // class="bg-transparent border-none"
                        tab_stop={false}
                        onClick={self.command_callback(WindowCommand::Minimize, cx)}
                        bg={cx.theme().transparent}
                        border_0
                    >
                        {icon(IconName::WindowMinimize, Some(12.), Some(theme.text_muted))}
                    </Button>
                    <Button
                        base={Button::new("window-maximize")}
                        tab_stop={false}
                        bg={cx.theme().transparent}
                        border_0
                        onClick={self.command_callback(WindowCommand::ToggleMaximize, cx)}
                    >
                        {icon(IconName::WindowMaximize, Some(12.), Some(theme.text_muted))}
                    </Button>
                    <Button
                        base={Button::new("window-close")}
                        tab_stop={false}
                        bg={cx.theme().transparent}
                        border_0
                        onClick={self.command_callback(WindowCommand::Close, cx)}
                    >
                        {icon(IconName::X, Some(12.), Some(theme.danger))}
                    </Button>
                </div>
            </div>
        }
    }
}

impl Render for WindowControls {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        rsx! {
            <div id="window-controls" relative flex flex_col w_full>
                {self.render_shell(cx)}
            </div>
        }
    }
}

const RESIZE_EDGE_THICKNESS: f32 = 6.;
const RESIZE_CORNER_SIZE: f32 = 12.;

fn resize_handle(
    id: &'static str,
    hitbox: Div,
    edge: ResizeEdge,
    cursor: CursorStyle,
) -> AnyElement {
    hitbox
        .id(id)
        .absolute()
        .cursor(cursor)
        .on_mouse_down(MouseButton::Left, move |_, window, _| {
            window.start_window_resize(edge);
        })
        .into_any_element()
}

/// Thin invisible hit-zones along the window's outer edge and corners.
///
/// `WindowDecorations::Client` (main.rs) means the OS/compositor draws no
/// native chrome — dragging the border to resize is entirely our
/// responsibility, same as the title bar's own move-by-drag below.
pub(crate) fn resize_handles() -> impl IntoElement {
    let edge_span = px(RESIZE_CORNER_SIZE);
    let edge_thickness = px(RESIZE_EDGE_THICKNESS);
    let corner = px(RESIZE_CORNER_SIZE);
    div()
        .id("window-resize-handles")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(resize_handle(
            "resize-top",
            div()
                .top_0()
                .left(edge_span)
                .right(edge_span)
                .h(edge_thickness),
            ResizeEdge::Top,
            CursorStyle::ResizeUpDown,
        ))
        .child(resize_handle(
            "resize-bottom",
            div()
                .bottom_0()
                .left(edge_span)
                .right(edge_span)
                .h(edge_thickness),
            ResizeEdge::Bottom,
            CursorStyle::ResizeUpDown,
        ))
        .child(resize_handle(
            "resize-left",
            div()
                .left_0()
                .top(edge_span)
                .bottom(edge_span)
                .w(edge_thickness),
            ResizeEdge::Left,
            CursorStyle::ResizeLeftRight,
        ))
        .child(resize_handle(
            "resize-right",
            div()
                .right_0()
                .top(edge_span)
                .bottom(edge_span)
                .w(edge_thickness),
            ResizeEdge::Right,
            CursorStyle::ResizeLeftRight,
        ))
        .child(resize_handle(
            "resize-top-left",
            div().top_0().left_0().size(corner),
            ResizeEdge::TopLeft,
            CursorStyle::ResizeUpLeftDownRight,
        ))
        .child(resize_handle(
            "resize-top-right",
            div().top_0().right_0().size(corner),
            ResizeEdge::TopRight,
            CursorStyle::ResizeUpRightDownLeft,
        ))
        .child(resize_handle(
            "resize-bottom-left",
            div().bottom_0().left_0().size(corner),
            ResizeEdge::BottomLeft,
            CursorStyle::ResizeUpRightDownLeft,
        ))
        .child(resize_handle(
            "resize-bottom-right",
            div().bottom_0().right_0().size(corner),
            ResizeEdge::BottomRight,
            CursorStyle::ResizeUpLeftDownRight,
        ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn title_bar_drag_is_scoped_to_explicit_left_button_spacers() {
        // The test platform's native window move is intentionally unimplemented, so a
        // compositor move cannot be observed here. Keep a source-level contract for the
        // listener's placement and button binding instead.
        let source = include_str!("controls.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("window-titlebar-drag-left"));
        assert!(production.contains("window-titlebar-drag-right"));
        assert_eq!(
            production
                .matches("onMouseDown={(MouseButton::Left, drag_")
                .count(),
            2
        );
        assert_eq!(production.matches("window.start_window_move()").count(), 2);
    }

    #[test]
    fn authenticated_lock_button_lives_in_title_bar_controls() {
        let source = include_str!("controls.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("let lock_button = if self.authenticated"));
        assert!(production.contains("Button::new(\"window-lock-vault\")"));
        assert!(production.contains("WindowCommand::LockVault"));
        assert!(production.contains("lock-keyhole-open"));
        assert!(production.contains("{lock_button}"));
    }

    #[test]
    fn authenticated_generate_password_button_lives_in_title_bar_controls() {
        let source = include_str!("controls.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("WindowCommand::GeneratePassword"));
        assert!(production.contains("Button::new(\"window-generate-password\")"));
        assert!(production.contains("wand-sparkles"));
        assert!(production.contains("{generate_password_button}"));
    }

    #[test]
    fn resize_handles_cover_all_four_edges_and_corners() {
        // Same reasoning as the drag test above: the test platform's resize is a
        // no-op, so this checks the source-level contract instead.
        let source = include_str!("controls.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for edge in [
            "Top",
            "Bottom",
            "Left",
            "Right",
            "TopLeft",
            "TopRight",
            "BottomLeft",
            "BottomRight",
        ] {
            assert!(
                production.contains(&format!("ResizeEdge::{edge}")),
                "missing a resize handle for {edge}"
            );
        }
        assert_eq!(
            production
                .matches("window.start_window_resize(edge)")
                .count(),
            1,
            "all 8 handles should share the one resize_handle() call site"
        );
    }
}
