use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use block2::{Block, ConcreteBlock, RcBlock};
use cacao::appkit::{App, AppDelegate};
use cacao::notification_center::Dispatcher;
use duct::cmd;
use icrate::ns_string;
use icrate::objc2::declare::Ivar;
use icrate::objc2::rc::Id;
use icrate::objc2::{
    declare::IvarDrop, declare_class, msg_send, msg_send_id, mutability, sel, ClassType,
};
use icrate::AppKit::{
    NSImage, NSMenuItem, NSStatusItem, NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
};
use icrate::AppKit::{NSStatusBar, NSVariableStatusItemLength};
use icrate::Foundation::{NSNotification, NSObject, NSString};
use once_cell::sync::OnceCell;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[allow(unused)]
struct SpaceResponse {
    id: u32,
    uuid: String,
    index: u32,
    r#type: YabaiSpaceLayout,
    label: String,
    display: u32,
}

struct Yabai {
    yabai: PathBuf,
}

impl Yabai {
    fn get_layout_for_current_space(&self) -> SpaceResponse {
        let output = cmd!(&self.yabai, "-m", "query", "--spaces", "--space")
            .read()
            .unwrap();

        serde_json::from_str(&output).unwrap()
    }

    fn change_space_layout(&self, layout: &YabaiSpaceLayout) {
        cmd!(&self.yabai, "-m", "space", "--layout", layout.to_string())
            .run()
            .unwrap();
    }
}

fn yabai() -> &'static Yabai {
    static INSTANCE: OnceCell<Yabai> = OnceCell::new();

    INSTANCE.get_or_init(|| {
        let yabai = PathBuf::from("/opt/homebrew/bin/yabai");
        Yabai { yabai }
    })
}

#[derive(Default, Deserialize, Debug, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
enum YabaiSpaceLayout {
    #[default]
    Float,
    Bsp,
}

impl ToString for YabaiSpaceLayout {
    fn to_string(&self) -> String {
        match self {
            YabaiSpaceLayout::Float => "float".to_string(),
            YabaiSpaceLayout::Bsp => "bsp".to_string(),
        }
    }
}

#[derive(Default)]
struct YabaiStateInner {
    layout: YabaiSpaceLayout,
}

#[derive(Clone)]
struct YabaiState {
    inner: Arc<(Mutex<YabaiStateInner>, Condvar)>,
}

impl YabaiState {
    fn shared() -> &'static Self {
        static INSTANCE: OnceCell<YabaiState> = OnceCell::new();

        INSTANCE.get_or_init(|| {
            let state = Self {
                inner: Arc::new((Mutex::new(YabaiStateInner::default()), Condvar::new())),
            };
            state.update();
            state
        })
    }

    fn set_layout(&self, new_layout: YabaiSpaceLayout) {
        let mut inner = self.inner.0.lock().unwrap();
        if new_layout != inner.layout {
            inner.layout = new_layout;
            self.inner.1.notify_all();

            App::<YabaiMenu, Message>::dispatch_main(Message::UpdateIcon);
        }
    }

    fn update(&self) {
        let new_layout = yabai().get_layout_for_current_space();
        self.set_layout(new_layout.r#type);
    }
}

struct YabaiMenuInner {
    status_item: RefCell<Option<Id<NSStatusItem>>>,
    action: RefCell<Option<Id<RustAction>>>,
}

#[derive(Debug)]
enum Message {
    UpdateIcon,
}

#[derive(Clone)]
struct YabaiMenu {
    inner: Arc<YabaiMenuInner>,
}

impl YabaiMenu {
    fn new() -> Self {
        Self {
            inner: Arc::new(YabaiMenuInner {
                status_item: RefCell::new(None),
                action: RefCell::new(None),
            }),
        }
    }

    fn get_icon(&self, layout: &YabaiSpaceLayout) -> &'static NSString {
        match layout {
            YabaiSpaceLayout::Float => ns_string!("macwindow.on.rectangle"),
            YabaiSpaceLayout::Bsp => ns_string!("uiwindow.split.2x1"),
        }
    }

    fn get_tooltip(&self, layout: &YabaiSpaceLayout) -> &'static NSString {
        match layout {
            YabaiSpaceLayout::Float => ns_string!("Float"),
            YabaiSpaceLayout::Bsp => ns_string!("BSP"),
        }
    }

    fn update_icon(&self) {
        unsafe {
            let layout = { YabaiState::shared().inner.0.lock().unwrap().layout.clone() };
            let icon_name = self.get_icon(&layout);
            let item = self.inner.status_item.borrow();
            let item = item.as_ref().unwrap();

            if let Some(button) = item.as_ref().button() {
                button.setImage(
                    NSImage::imageWithSystemSymbolName_accessibilityDescription(
                        icon_name,
                        Some(ns_string!("1")),
                    )
                    .as_deref(),
                );
                button.setToolTip(Some(self.get_tooltip(&layout)));
            }
        }
    }

    fn toggle_layout(&self) {
        let layout = {
            let inner = YabaiState::shared().inner.0.lock().unwrap();
            match inner.layout {
                YabaiSpaceLayout::Float => YabaiSpaceLayout::Bsp,
                YabaiSpaceLayout::Bsp => YabaiSpaceLayout::Float,
            }
        };
        yabai().change_space_layout(&layout);
        YabaiState::shared().set_layout(layout);
    }
}

impl Dispatcher for YabaiMenu {
    type Message = Message;

    fn on_ui_message(&self, message: Self::Message) {
        match message {
            Message::UpdateIcon => self.update_icon(),
        }
    }
}

impl AppDelegate for YabaiMenu {
    fn did_finish_launching(&self) {
        let this = self.clone();
        unsafe {
            let menubar = NSStatusBar::systemStatusBar();
            let item = menubar.statusItemWithLength(NSVariableStatusItemLength);
            if let Some(button) = item.button() {
                let action = RustAction::new(move || {
                    this.toggle_layout();
                });
                button.setTarget(Some(action.as_ref()));
                button.setAction(Some(sel!(call:)));
                *self.inner.action.borrow_mut() = Some(action);
            }
            *self.inner.status_item.borrow_mut() = Some(item);
            self.update_icon();
        }
    }
}

declare_class!(
    struct WorkspaceObserver {}

    unsafe impl ClassType for WorkspaceObserver {
        type Super = NSObject;
        type Mutability = mutability::Mutable;
        const NAME: &'static str = "YBWorkspaceObserver";
    }

    unsafe impl WorkspaceObserver {
        #[method(init)]
        fn init(this: &mut Self) -> Option<&mut Self> {
            let this: Option<&mut Self> = unsafe { msg_send![super(this), init] };
            this
        }

        #[method(observeActiveSpaceDidChangeNotification:)]
        #[allow(non_snake_case)]
        fn __observeActiveSpaceDidChangeNotification(
            _this: &mut Self,
            _notification: *const NSNotification,
        ) {
            YabaiState::shared().update();
        }
    }
);

impl WorkspaceObserver {
    pub fn new() -> Id<Self> {
        unsafe { msg_send_id![Self::alloc(), init] }
    }
}

declare_class!(
    #[derive(Debug)]
    struct RustAction {
        callback: IvarDrop<Box<RcBlock<(), ()>>, "_callback">,
    }

    mod ivars;

    unsafe impl ClassType for RustAction {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "RustAction";
    }

    unsafe impl RustAction {
        #[method(initWithCallback:)]
        unsafe fn init(this: *mut Self, callback: *mut Block<(), ()>) -> Option<NonNull<Self>> {
            let this: Option<&mut Self> = msg_send![super(this), init];
            let Some(this) = this else {
                return None;
            };

            Ivar::write(&mut this.callback, Box::new(RcBlock::copy(callback)));

            Some(NonNull::from(this))
        }

        #[method(call:)]
        unsafe fn call(&self, _sender: *mut NSMenuItem) {
            self.callback.call(());
        }
    }
);

impl RustAction {
    pub fn new<F: Fn() + 'static>(callback: F) -> Id<Self> {
        unsafe {
            let block = Box::into_raw(Box::new(ConcreteBlock::new(callback)));
            msg_send_id![Self::alloc(), initWithCallback: block]
        }
    }
}

fn main() {
    let state = YabaiState::shared();

    let delegate = YabaiMenu::new();

    // state update loop
    let _state = thread::spawn({
        let state = state.clone();
        move || loop {
            thread::sleep(Duration::from_secs(1));
            state.update();
        }
    });

    let observer = WorkspaceObserver::new();

    unsafe {
        NSWorkspace::sharedWorkspace()
            .notificationCenter()
            .addObserver_selector_name_object(
                &observer.as_ref(),
                sel!(observeActiveSpaceDidChangeNotification:),
                Some(NSWorkspaceActiveSpaceDidChangeNotification),
                None,
            );
    }

    App::new("fan.zeyi.yabai-menu", delegate.clone()).run();
}
