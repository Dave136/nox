use gpui::{
    App, Context, Entity, FocusHandle, Focusable, Keystroke, MouseButton, MouseDownEvent, Window,
    actions, div, hsla, prelude::*, px, rgb,
};
use gpui_component::{
    ActiveTheme, Sizable, WindowExt,
    button::Button,
    command::{Command, CommandItem, CommandState},
    kbd::Kbd,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_rsx::rsx;
use std::{cell::Cell, rc::Rc};

use crate::{
    app::{CIPHER_BACKGROUND, CIPHER_FOREGROUND_MUTED},
    assets::{IconName, icon, logo},
};

const CIPHER_BORDER: u32 = 0x292D35;
const CIPHER_FOREGROUND: u32 = 0xE5E8F0;
const CIPHER_MUTED: u32 = 0x8F98A8;
const CIPHER_DANGER: u32 = 0xA9787D;

// use crate::assets::{IconName};

actions!(window_controls, [OpenCommandPalette]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowCommand {
    Minimize,
    ToggleMaximize,
    Close,
}

impl WindowCommand {
    pub const ALL: [Self; 3] = [Self::Minimize, Self::ToggleMaximize, Self::Close];

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Minimize => "Minimize",
            Self::ToggleMaximize => "Maximize / Restore",
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
    /// the owning `Locker` view so this always reflects `AppState`, with no
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

    fn render_file_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = cx.entity().downgrade();
        rsx! {
            <Button
                base={Button::new("window-file-menu")}
                label={"File"}
                bg={cx.theme().transparent}
                textColor={rgb(CIPHER_MUTED)}
                border_0
                // Keep title-bar controls out of the vault form's tab order; Ctrl+P
                // provides the keyboard route to every native window command.
                tab_stop={false}
                small
                dropdown_menu={move |menu, _, _| {
                    let controls = controls.clone();
                    menu.item(PopupMenuItem::new("New Vault").disabled(true))
                        .item(PopupMenuItem::new("Open Vault").disabled(true))
                        .item(PopupMenuItem::new("Lock Vault").disabled(true))
                        .separator()
                        .item(
                            PopupMenuItem::new("Close Window").on_click(move |_, window, app| {
                                let _ = controls.update(app, |controls, cx| {
                                    controls.invoke_command(WindowCommand::Close, window, cx);
                                });
                            }),
                        )
                }}
            />
        }
    }

    fn render_help_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        rsx! {
            <Button
                base={Button::new("window-help-menu")}
                label={"Help"}
                bg={cx.theme().transparent}
                textColor={rgb(CIPHER_MUTED)}
                border_0
                // Keep title-bar controls out of the vault form's tab order; Ctrl+P
                // provides the keyboard route to every native window command.
                tab_stop={false}
                small
                dropdown_menu={|menu, _, _| {
                    menu.item(PopupMenuItem::new("Keyboard Shortcuts").disabled(true))
                        .item(PopupMenuItem::new("About Locker").disabled(true))
                }}
            />
        }
    }

    fn render_shell(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open =
            cx.listener(|this, _: &gpui::ClickEvent, window, cx| this.open_palette(window, cx));
        let drag_left = cx.listener(|_, _: &MouseDownEvent, window, _| {
            window.start_window_move();
        });
        let drag_right = cx.listener(|_, _: &MouseDownEvent, window, _| {
            window.start_window_move();
        });
        let search_trigger = if self.authenticated {
            rsx! {
                <div items_center justify_center gap={px(8.)}>
                    <Button
                        base={Button::new("window-command-palette-trigger")
                            .debug_selector(|| "window-command-palette-trigger".to_owned())}
                        min_w={px(200.)}
                        onClick={open}
                        bg={rgb(CIPHER_BACKGROUND)}
                        border_color={cx.theme().input}
                        tab_stop={false}
                    >
                        <div flex items_center w_full gap={px(8.)}>
                            {icon(IconName::Search, Some(14.), Some(rgb(CIPHER_FOREGROUND_MUTED).into()))}
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
                bg={rgb(CIPHER_BACKGROUND)}
                border_b_1
                borderColor={rgb(CIPHER_BORDER)}
            >
                <div flex items_center gap={px(4.)}>
                    <div flex items_center gap={px(7.)} mr={px(8.)} textColor={rgb(CIPHER_FOREGROUND)}>
                        {logo(18., rgb(CIPHER_FOREGROUND).into())}
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
                    <Button
                        base={Button::new("window-minimize")}
                        // class="bg-transparent border-none"
                        tab_stop={false}
                        onClick={self.command_callback(WindowCommand::Minimize, cx)}
                        bg={cx.theme().transparent}
                        border_0
                    >
                        {icon(IconName::WindowMinimize, Some(12.), Some(rgb(CIPHER_MUTED).into()))}
                    </Button>
                    <Button
                        base={Button::new("window-maximize")}
                        tab_stop={false}
                        bg={cx.theme().transparent}
                        border_0
                        onClick={self.command_callback(WindowCommand::ToggleMaximize, cx)}
                    >
                        {icon(IconName::WindowMaximize, Some(12.), Some(rgb(CIPHER_MUTED).into()))}
                    </Button>
                    <Button
                        base={Button::new("window-close")}
                        tab_stop={false}
                        bg={cx.theme().transparent}
                        border_0
                        onClick={self.command_callback(WindowCommand::Close, cx)}
                    >
                        {icon(IconName::X, Some(12.), Some(rgb(CIPHER_DANGER).into()))}
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
}
