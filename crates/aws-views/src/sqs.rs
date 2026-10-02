//! Queue browsing and message operations for the AWS client.

use aws_sqs::{
    Account, AwsCli, IdentityCenter, LoginMethod, LoginSettings, Message, QueueAttributes, Role,
    RoleCredentials, SsoSession, parse_string_message_attributes,
};
use ginka_core::settings;
use ginka_ui::Tokens;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Disableable, IconName, Sizable as _, h_flex, v_flex};
use std::path::PathBuf;

/// SQS surface shared by the standalone AWS window and any other GPUI host.
pub struct SqsView {
    settings_path: Option<PathBuf>,
    show_connection_settings: bool,
    start_url: Entity<InputState>,
    sso_region: Entity<InputState>,
    region: Entity<InputState>,
    queue_url: Entity<InputState>,
    new_queue_name: Entity<InputState>,
    queue_filter: Entity<InputState>,
    body: Entity<TextareaState>,
    message_attributes: Entity<TextareaState>,
    send_delay: Entity<InputState>,
    queue_delay: Entity<InputState>,
    queue_visibility_timeout: Entity<InputState>,
    queue_receive_wait: Entity<InputState>,
    queue_message_retention: Entity<InputState>,
    group_id: Entity<InputState>,
    deduplication_id: Entity<InputState>,
    receive_visibility_timeout: Entity<InputState>,
    receive_count: Entity<InputState>,
    message_visibility_timeout: Entity<InputState>,
    queue_state: QueueState,
    queues_loaded: bool,
    needs_refresh: bool,
    busy: bool,
    login_busy: bool,
    login_method: LoginMethod,
    console_connected: bool,
    auth_flow: AuthFlow,
    login_url: Option<String>,
    login_code: Option<String>,
    login_url_opened: bool,
    login_feedback: Option<(String, bool)>,
    session: Option<SsoSession>,
    accounts: Vec<Account>,
    roles: Vec<Role>,
    selected_account: Option<Account>,
    selected_role: Option<Role>,
    credentials: Option<RoleCredentials>,
    auth_generation: u64,
    receive_wait: ReceiveWait,
    status: Option<(String, bool)>,
    clear_body: Option<String>,
    clear_message_attributes: Option<String>,
    clear_queue_delay: Option<String>,
    clear_queue_visibility_timeout: Option<String>,
    clear_queue_receive_wait: Option<String>,
    clear_queue_message_retention: Option<String>,
}

#[derive(Default)]
struct QueueState {
    queues: Vec<String>,
    selected: Option<String>,
    attributes: Option<QueueAttributes>,
    messages: Vec<Message>,
    pending_delete: Option<String>,
    pending_queue_action: Option<QueueAction>,
    pending_message_retention: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueAction {
    Purge,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthFlow {
    Pkce,
    Device,
}

#[derive(Clone, Copy)]
enum ReceiveWait {
    Short,
    Long,
    QueueDefault,
}

impl ReceiveWait {
    fn next(self) -> Self {
        match self {
            Self::Short => Self::Long,
            Self::Long => Self::QueueDefault,
            Self::QueueDefault => Self::Short,
        }
    }

    fn seconds(self) -> Option<u8> {
        match self {
            Self::Short => Some(0),
            Self::Long => Some(20),
            Self::QueueDefault => None,
        }
    }
}

impl QueueState {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

impl SqsView {
    /// Create the surface; queues load after Console or Identity Center sign-in.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(window, cx, None)
    }

    /// Create the surface with a file for non-secret connection settings.
    pub fn with_settings_file(
        window: &mut Window,
        cx: &mut Context<Self>,
        settings_path: PathBuf,
    ) -> Self {
        Self::build(window, cx, Some(settings_path))
    }

    fn build(window: &mut Window, cx: &mut Context<Self>, settings_path: Option<PathBuf>) -> Self {
        let start_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.start_url_placeholder").to_string())
        });
        let sso_region = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sso_region_placeholder").to_string())
        });
        let region = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.region_placeholder").to_string())
        });
        let queue_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.queue_url_placeholder").to_string())
        });
        let new_queue_name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.new_queue_name").to_string())
        });
        let queue_filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.filter_placeholder").to_string())
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.message_body").to_string())
                .auto_grow(6, 240)
        });
        let message_attributes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.message_attributes_placeholder").to_string())
                .auto_grow(3, 140)
        });
        let send_delay = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.send_delay_placeholder").to_string())
        });
        let queue_delay = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.queue_delay_placeholder").to_string())
        });
        let queue_visibility_timeout = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.queue_visibility_placeholder").to_string())
        });
        let queue_receive_wait = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.queue_receive_wait_placeholder").to_string())
        });
        let queue_message_retention = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                rust_i18n::t!("aws.sqs.queue_message_retention_placeholder").to_string(),
            )
        });
        let group_id = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("aws.sqs.group_id").to_string())
        });
        let deduplication_id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.deduplication_id").to_string())
        });
        let receive_visibility_timeout = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.visibility_timeout_placeholder").to_string())
        });
        let receive_count = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.receive_count_placeholder").to_string())
        });
        receive_count.update(cx, |input, cx| input.set_value("10", window, cx));
        let message_visibility_timeout = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("aws.sqs.message_visibility_placeholder").to_string())
        });
        let saved: LoginSettings = settings_path
            .as_deref()
            .map(settings::load)
            .unwrap_or_default();
        let login_method = if settings_path.as_ref().is_some_and(|path| path.exists()) {
            saved.method
        } else {
            LoginMethod::Console
        };
        if settings_path.is_some() {
            start_url.update(cx, |input, cx| input.set_value(saved.start_url, window, cx));
            sso_region.update(cx, |input, cx| {
                input.set_value(saved.sso_region, window, cx)
            });
            region.update(cx, |input, cx| input.set_value(saved.region, window, cx));
        }
        if let Ok(value) = std::env::var("AWS_SSO_START_URL") {
            start_url.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if let Ok(value) = std::env::var("AWS_SSO_REGION") {
            sso_region.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if let Ok(value) =
            std::env::var("AWS_REGION").or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        {
            region.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if region.read(cx).value().trim().is_empty() {
            region.update(cx, |input, cx| {
                input.set_value("ap-northeast-1", window, cx)
            });
        }
        let show_connection_settings = !LoginSettings {
            method: login_method,
            start_url: start_url.read(cx).value().to_string(),
            sso_region: sso_region.read(cx).value().to_string(),
            region: region.read(cx).value().to_string(),
        }
        .is_complete();
        for input in [&start_url, &sso_region, &region] {
            cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.queue_state.clear();
                    this.queues_loaded = false;
                    this.clear_queue_delay = Some(this.queue_delay.read(cx).value().to_string());
                    this.clear_queue_visibility_timeout =
                        Some(this.queue_visibility_timeout.read(cx).value().to_string());
                    this.clear_queue_receive_wait =
                        Some(this.queue_receive_wait.read(cx).value().to_string());
                    this.clear_queue_message_retention =
                        Some(this.queue_message_retention.read(cx).value().to_string());
                    this.needs_refresh = true;
                    this.status = None;
                    this.auth_generation = this.auth_generation.wrapping_add(1);
                    this.session = None;
                    this.accounts.clear();
                    this.roles.clear();
                    this.selected_account = None;
                    this.selected_role = None;
                    this.credentials = None;
                    this.console_connected = false;
                    cx.notify();
                }
            })
            .detach();
        }
        cx.subscribe(&queue_filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(
            &queue_message_retention,
            |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.queue_state.pending_message_retention = None;
                    cx.notify();
                }
            },
        )
        .detach();
        Self {
            settings_path,
            show_connection_settings,
            start_url,
            sso_region,
            region,
            queue_url,
            new_queue_name,
            queue_filter,
            body,
            message_attributes,
            send_delay,
            queue_delay,
            queue_visibility_timeout,
            queue_receive_wait,
            queue_message_retention,
            group_id,
            deduplication_id,
            receive_visibility_timeout,
            receive_count,
            message_visibility_timeout,
            queue_state: QueueState::default(),
            queues_loaded: false,
            needs_refresh: false,
            busy: false,
            login_busy: false,
            login_method,
            console_connected: false,
            auth_flow: AuthFlow::Pkce,
            login_url: None,
            login_code: None,
            login_url_opened: false,
            login_feedback: None,
            session: None,
            accounts: Vec::new(),
            roles: Vec::new(),
            selected_account: None,
            selected_role: None,
            credentials: None,
            auth_generation: 0,
            receive_wait: ReceiveWait::Short,
            status: None,
            clear_body: None,
            clear_message_attributes: None,
            clear_queue_delay: None,
            clear_queue_visibility_timeout: None,
            clear_queue_receive_wait: None,
            clear_queue_message_retention: None,
        }
    }

    fn client(&self, cx: &App) -> AwsCli {
        if self.console_connected {
            return self
                .console_client(cx)
                .unwrap_or_else(|_| AwsCli::unauthenticated());
        }
        self.credentials
            .as_ref()
            .and_then(|credentials| {
                AwsCli::native(self.region.read(cx).value().trim(), credentials.clone()).ok()
            })
            .unwrap_or_else(AwsCli::unauthenticated)
    }

    fn console_client(&self, cx: &App) -> Result<AwsCli, aws_sqs::Error> {
        let home = self
            .settings_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|path| path.join("aws-console"))
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("ginka-aws-{}", std::process::id()))
            });
        AwsCli::console(self.region.read(cx).value().trim(), home)
    }

    fn save_connection(&self, cx: &App) {
        if let Some(path) = self.settings_path.as_deref() {
            let connection = LoginSettings {
                method: self.login_method,
                start_url: self.start_url.read(cx).value().trim().to_string(),
                sso_region: self.sso_region.read(cx).value().trim().to_string(),
                region: self.region.read(cx).value().trim().to_string(),
            };
            if let Err(error) = settings::save(path, &connection) {
                eprintln!("Could not save AWS connection settings: {error}");
            }
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.credentials.is_none() && !self.console_connected {
            self.queues_loaded = false;
            return;
        }
        if self.busy {
            return;
        }
        self.busy = true;
        self.needs_refresh = false;
        if !self.login_busy {
            self.status = None;
        }
        self.queue_state.clear();
        self.clear_queue_delay = Some(self.queue_delay.read(cx).value().to_string());
        self.clear_queue_visibility_timeout =
            Some(self.queue_visibility_timeout.read(cx).value().to_string());
        self.clear_queue_receive_wait = Some(self.queue_receive_wait.read(cx).value().to_string());
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { smol::unblock(move || client.list_queues()).await })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(queues) => {
                        this.queue_state.queues = queues;
                        this.queues_loaded = true;
                    }
                    Err(error) if !this.login_busy => {
                        this.queues_loaded = false;
                        this.status = Some((error.to_string(), true));
                    }
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn login(&mut self, cx: &mut Context<Self>) {
        if self.login_busy {
            return;
        }
        if self.login_method == LoginMethod::Console {
            self.login_console(cx);
            return;
        }
        let start_url = self.start_url.read(cx).value().trim().to_string();
        let sso_region = self.sso_region.read(cx).value().trim().to_string();
        let region = self.region.read(cx).value().trim().to_string();
        let auth_flow = self.auth_flow;
        if start_url.is_empty() || sso_region.is_empty() || region.is_empty() {
            self.show_connection_settings = true;
            self.login_feedback =
                Some((rust_i18n::t!("aws.login_fields_required").to_string(), true));
            cx.notify();
            return;
        }
        self.login_busy = true;
        self.auth_generation = self.auth_generation.wrapping_add(1);
        let generation = self.auth_generation;
        self.session = None;
        self.accounts.clear();
        self.roles.clear();
        self.selected_account = None;
        self.selected_role = None;
        self.credentials = None;
        self.login_url = None;
        self.login_code = None;
        self.login_url_opened = false;
        self.login_feedback = Some((rust_i18n::t!("aws.login_in_progress").to_string(), false));
        cx.spawn(async move |this, cx| {
            let (sender, receiver) = smol::channel::unbounded::<(String, Option<String>)>();
            let login_task = cx.background_spawn(async move {
                smol::unblock(move || {
                    let identity = IdentityCenter::new();
                    let session = match auth_flow {
                        AuthFlow::Pkce => {
                            identity.sign_in_pkce(&start_url, &sso_region, |url| {
                                let _ = sender.send_blocking((url, None));
                            })?
                        }
                        AuthFlow::Device => {
                            identity.sign_in(&start_url, &sso_region, |device| {
                                let _ = sender.send_blocking((
                                    device.verification_uri,
                                    Some(device.user_code),
                                ));
                            })?
                        }
                    };
                    let accounts = identity.accounts(&session)?;
                    Ok::<_, aws_sqs::Error>((session, accounts))
                })
                .await
            });
            while let Ok((url, code)) = receiver.recv().await {
                this.update(cx, |this, cx| {
                    if this.auth_generation != generation {
                        return;
                    }
                    this.login_url = Some(url);
                    this.login_code = code;
                    if !this.login_url_opened
                        && let Some(url) = this.login_url.as_deref()
                    {
                        cx.open_url(url);
                        this.login_url_opened = true;
                    }
                    cx.notify();
                })
                .ok();
            }
            let result = login_task.await;
            this.update(cx, |this, cx| {
                if this.auth_generation != generation {
                    return;
                }
                this.login_busy = false;
                match result {
                    Ok((session, accounts)) => {
                        this.save_connection(cx);
                        this.show_connection_settings = false;
                        this.session = Some(session);
                        this.accounts = accounts;
                        this.login_url = None;
                        this.login_code = None;
                        this.login_feedback =
                            Some((rust_i18n::t!("aws.choose_account").to_string(), false));
                        if this.accounts.len() == 1 {
                            let account = this.accounts[0].clone();
                            this.select_account(account, cx);
                        }
                    }
                    Err(error) => {
                        let message = format!("{} {error}", rust_i18n::t!("aws.login_failed"));
                        this.login_feedback = Some((message.clone(), true));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn login_console(&mut self, cx: &mut Context<Self>) {
        let client = match self.console_client(cx) {
            Ok(client) => client,
            Err(error) => {
                self.login_feedback = Some((error.to_string(), true));
                cx.notify();
                return;
            }
        };
        self.login_busy = true;
        self.console_connected = false;
        self.auth_generation = self.auth_generation.wrapping_add(1);
        let generation = self.auth_generation;
        self.login_feedback = Some((rust_i18n::t!("aws.login_in_progress").to_string(), false));
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { smol::unblock(move || client.console_login()).await },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.auth_generation != generation {
                    return;
                }
                this.login_busy = false;
                match result {
                    Ok(()) => {
                        this.console_connected = true;
                        this.login_feedback = None;
                        this.show_connection_settings = false;
                        this.save_connection(cx);
                        this.reload(cx);
                    }
                    Err(error) => {
                        this.login_feedback = Some((
                            format!("{} {error}", rust_i18n::t!("aws.login_failed")),
                            true,
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn select_login_method(&mut self, method: LoginMethod, cx: &mut Context<Self>) {
        if self.login_method == method {
            return;
        }
        self.auth_generation = self.auth_generation.wrapping_add(1);
        self.login_method = method;
        self.console_connected = false;
        self.credentials = None;
        self.session = None;
        self.accounts.clear();
        self.roles.clear();
        self.selected_account = None;
        self.selected_role = None;
        self.queue_state.clear();
        self.queues_loaded = false;
        self.login_feedback = None;
        self.login_url = None;
        self.login_code = None;
        cx.notify();
    }

    fn select_account(&mut self, account: Account, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.auth_generation = self.auth_generation.wrapping_add(1);
        let generation = self.auth_generation;
        self.selected_account = Some(account.clone());
        self.selected_role = None;
        self.credentials = None;
        self.roles.clear();
        self.login_busy = true;
        self.login_feedback = Some((rust_i18n::t!("aws.loading_roles").to_string(), false));
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || {
                        IdentityCenter::new().roles(&session, &account.account_id)
                    })
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                if this.auth_generation != generation {
                    return;
                }
                this.login_busy = false;
                match result {
                    Ok(roles) => {
                        this.roles = roles;
                        this.login_feedback =
                            Some((rust_i18n::t!("aws.choose_role").to_string(), false));
                        if this.roles.len() == 1 {
                            let role = this.roles[0].clone();
                            this.select_role(role, cx);
                        }
                    }
                    Err(error) => this.login_feedback = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn select_role(&mut self, role: Role, cx: &mut Context<Self>) {
        let (Some(session), Some(account)) = (self.session.clone(), self.selected_account.clone())
        else {
            return;
        };
        self.auth_generation = self.auth_generation.wrapping_add(1);
        let generation = self.auth_generation;
        self.selected_role = Some(role.clone());
        self.login_busy = true;
        self.login_feedback = Some((rust_i18n::t!("aws.loading_credentials").to_string(), false));
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || {
                        IdentityCenter::new().role_credentials(
                            &session,
                            &account.account_id,
                            &role.role_name,
                        )
                    })
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                if this.auth_generation != generation {
                    return;
                }
                this.login_busy = false;
                match result {
                    Ok(credentials) => {
                        match AwsCli::native(
                            this.region.read(cx).value().trim(),
                            credentials.clone(),
                        ) {
                            Ok(_) => {
                                this.credentials = Some(credentials);
                                this.login_feedback = None;
                                this.reload(cx);
                            }
                            Err(error) => {
                                this.login_feedback = Some((error.to_string(), true));
                            }
                        }
                    }
                    Err(error) => this.login_feedback = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn select(&mut self, queue: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.queue_state.selected = Some(queue.clone());
        self.queue_state.messages.clear();
        self.queue_state.pending_delete = None;
        self.queue_state.attributes = None;
        self.queue_state.pending_queue_action = None;
        self.queue_state.pending_message_retention = None;
        self.clear_queue_delay = Some(self.queue_delay.read(cx).value().to_string());
        self.clear_queue_visibility_timeout =
            Some(self.queue_visibility_timeout.read(cx).value().to_string());
        self.clear_queue_receive_wait = Some(self.queue_receive_wait.read(cx).value().to_string());
        self.clear_queue_message_retention =
            Some(self.queue_message_retention.read(cx).value().to_string());
        self.status = None;
        self.refresh_attributes(queue, cx);
    }

    fn open_queue_url(&mut self, cx: &mut Context<Self>) {
        let queue = self.queue_url.read(cx).value().trim().to_string();
        if queue.is_empty() {
            self.status = Some((
                rust_i18n::t!("aws.sqs.queue_url_required").to_string(),
                true,
            ));
            cx.notify();
            return;
        }
        self.select(queue, cx);
    }

    fn create_queue(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let name = self.new_queue_name.read(cx).value().to_string();
        self.queue_state.pending_queue_action = None;
        self.busy = true;
        self.status = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { smol::unblock(move || client.create_queue(&name)).await },
                )
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(url) => {
                        if !this.queue_state.queues.contains(&url) {
                            this.queue_state.queues.push(url.clone());
                            this.queue_state.queues.sort();
                        }
                        this.select(url, cx);
                        this.status = Some((rust_i18n::t!("aws.sqs.created").to_string(), false));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_attributes(&mut self, queue: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.queue_state.pending_queue_action = None;
        self.busy = true;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || client.queue_attributes(&queue)).await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(attributes) => {
                        this.queue_state.attributes = Some(attributes);
                        this.status = None;
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_queue_delay(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        let value = self.queue_delay.read(cx).value().trim().to_string();
        let seconds = match value.parse::<u32>() {
            Ok(seconds) if seconds <= 900 => seconds,
            _ => {
                self.status = Some((
                    rust_i18n::t!("aws.sqs.invalid_queue_delay").to_string(),
                    true,
                ));
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.status = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || client.set_queue_delay(&queue, seconds)).await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(attributes) = this.queue_state.attributes.as_mut() {
                            attributes.delay_seconds = Some(seconds);
                        }
                        this.clear_queue_delay = Some(value);
                        this.status = Some((
                            rust_i18n::t!("aws.sqs.queue_delay_changed", seconds = seconds)
                                .to_string(),
                            false,
                        ));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_queue_visibility_timeout(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        let value = self
            .queue_visibility_timeout
            .read(cx)
            .value()
            .trim()
            .to_string();
        let seconds = match value.parse::<u32>() {
            Ok(seconds) if seconds <= 43_200 => seconds,
            _ => {
                self.status = Some((
                    rust_i18n::t!("aws.sqs.invalid_queue_visibility").to_string(),
                    true,
                ));
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.status = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || client.set_queue_visibility_timeout(&queue, seconds))
                        .await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(attributes) = this.queue_state.attributes.as_mut() {
                            attributes.visibility_timeout = Some(seconds);
                        }
                        this.clear_queue_visibility_timeout = Some(value);
                        this.status = Some((
                            rust_i18n::t!("aws.sqs.queue_visibility_changed", seconds = seconds)
                                .to_string(),
                            false,
                        ));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_queue_receive_wait(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        let value = self.queue_receive_wait.read(cx).value().trim().to_string();
        let seconds = match value.parse::<u8>() {
            Ok(seconds) if seconds <= 20 => seconds,
            _ => {
                self.status = Some((
                    rust_i18n::t!("aws.sqs.invalid_queue_receive_wait").to_string(),
                    true,
                ));
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.status = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || client.set_queue_receive_wait(&queue, seconds)).await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(attributes) = this.queue_state.attributes.as_mut() {
                            attributes.receive_wait_seconds = Some(seconds);
                        }
                        this.clear_queue_receive_wait = Some(value);
                        this.status = Some((
                            rust_i18n::t!("aws.sqs.queue_receive_wait_changed", seconds = seconds)
                                .to_string(),
                            false,
                        ));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_queue_message_retention(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        let value = self
            .queue_message_retention
            .read(cx)
            .value()
            .trim()
            .to_string();
        let seconds = match value.parse::<u32>() {
            Ok(seconds @ 60..=1_209_600) => seconds,
            _ => {
                self.queue_state.pending_message_retention = None;
                self.status = Some((
                    rust_i18n::t!("aws.sqs.invalid_queue_message_retention").to_string(),
                    true,
                ));
                cx.notify();
                return;
            }
        };
        if self.queue_state.pending_message_retention != Some(seconds) {
            self.queue_state.pending_message_retention = Some(seconds);
            self.status = Some((
                rust_i18n::t!("aws.sqs.queue_message_retention_warning").to_string(),
                false,
            ));
            cx.notify();
            return;
        }
        self.queue_state.pending_message_retention = None;
        self.busy = true;
        self.status = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || {
                        client.set_queue_message_retention_period(&queue, seconds)
                    })
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(attributes) = this.queue_state.attributes.as_mut() {
                            attributes.message_retention_period = Some(seconds);
                        }
                        this.clear_queue_message_retention = Some(value);
                        this.status = Some((
                            rust_i18n::t!(
                                "aws.sqs.queue_message_retention_changed",
                                seconds = seconds
                            )
                            .to_string(),
                            false,
                        ));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn receive(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        let count = match self.receive_count.read(cx).value().trim().parse::<u8>() {
            Ok(count @ 1..=10) => count,
            _ => {
                self.status = Some((
                    rust_i18n::t!("aws.sqs.invalid_receive_count").to_string(),
                    true,
                ));
                cx.notify();
                return;
            }
        };
        let raw_visibility = self.receive_visibility_timeout.read(cx).value().to_string();
        let raw_visibility = raw_visibility.trim();
        let visibility_timeout = if raw_visibility.is_empty() {
            None
        } else {
            match raw_visibility.parse::<u32>() {
                Ok(seconds) if seconds <= 43_200 => Some(seconds),
                _ => {
                    self.status = Some((
                        rust_i18n::t!("aws.sqs.invalid_visibility_timeout").to_string(),
                        true,
                    ));
                    cx.notify();
                    return;
                }
            }
        };
        self.busy = true;
        self.status = None;
        self.queue_state.pending_delete = None;
        self.queue_state.pending_queue_action = None;
        let client = self.client(cx);
        let wait_seconds = self.receive_wait.seconds();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || {
                        client.receive_messages_with_settings(
                            &queue,
                            count,
                            wait_seconds,
                            visibility_timeout,
                        )
                    })
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(messages) => {
                        let count = messages.len();
                        this.queue_state.messages = messages;
                        this.status = Some((
                            rust_i18n::t!("aws.sqs.received", count = count).to_string(),
                            false,
                        ));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        let body = self.body.read(cx).value().to_string();
        let sent_body = body.clone();
        let attributes_text = self.message_attributes.read(cx).value().to_string();
        let attributes = match parse_string_message_attributes(&attributes_text) {
            Ok(attributes) => attributes,
            Err(error) => {
                self.status = Some((error.to_string(), true));
                cx.notify();
                return;
            }
        };
        let group = self.group_id.read(cx).value().to_string();
        let dedup = self.deduplication_id.read(cx).value().to_string();
        let delay_text = self.send_delay.read(cx).value().trim().to_string();
        let delay = if queue.ends_with(".fifo") || delay_text.is_empty() {
            None
        } else {
            match delay_text.parse::<u32>() {
                Ok(seconds) if seconds <= 900 => Some(seconds),
                _ => {
                    self.status = Some((
                        rust_i18n::t!("aws.sqs.invalid_send_delay").to_string(),
                        true,
                    ));
                    cx.notify();
                    return;
                }
            }
        };
        self.busy = true;
        self.status = None;
        self.queue_state.pending_queue_action = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || {
                        client.send_message_with_attributes(
                            &queue,
                            &body,
                            Some(&group),
                            Some(&dedup),
                            delay,
                            &attributes,
                        )
                    })
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.clear_body = Some(sent_body);
                        this.clear_message_attributes = Some(attributes_text);
                        this.status = Some((rust_i18n::t!("aws.sqs.sent").to_string(), false));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn delete(&mut self, receipt: String, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.queue_state.pending_queue_action = None;
        if self.queue_state.pending_delete.as_deref() != Some(receipt.as_str()) {
            self.queue_state.pending_delete = Some(receipt);
            cx.notify();
            return;
        }
        self.queue_state.pending_delete = None;
        self.queue_state.pending_queue_action = None;
        self.busy = true;
        self.status = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let receipt_for_delete = receipt.clone();
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || client.delete_message(&queue, &receipt_for_delete)).await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.queue_state
                            .messages
                            .retain(|message| message.receipt_handle != receipt);
                        this.status = Some((rust_i18n::t!("aws.sqs.deleted").to_string(), false));
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_message_visibility(&mut self, receipt: String, cx: &mut Context<Self>) {
        let raw = self.message_visibility_timeout.read(cx).value().to_string();
        let seconds = match raw.trim().parse::<u32>() {
            Ok(seconds) if seconds <= 43_200 => seconds,
            _ => {
                self.status = Some((
                    rust_i18n::t!("aws.sqs.invalid_visibility_timeout").to_string(),
                    true,
                ));
                cx.notify();
                return;
            }
        };
        self.change_message_visibility(receipt, seconds, cx);
    }

    fn change_message_visibility(&mut self, receipt: String, seconds: u32, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = None;
        self.queue_state.pending_delete = None;
        self.queue_state.pending_queue_action = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let receipt_for_change = receipt.clone();
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || {
                        client.change_message_visibility(&queue, &receipt_for_change, seconds)
                    })
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if seconds == 0 {
                            this.queue_state
                                .messages
                                .retain(|message| message.receipt_handle != receipt);
                            this.status =
                                Some((rust_i18n::t!("aws.sqs.released").to_string(), false));
                        } else {
                            this.status = Some((
                                rust_i18n::t!("aws.sqs.visibility_changed", seconds = seconds)
                                    .to_string(),
                                false,
                            ));
                        }
                    }
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn queue_action(&mut self, action: QueueAction, cx: &mut Context<Self>) {
        let Some(queue) = self.queue_state.selected.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        if self.queue_state.pending_queue_action != Some(action) {
            self.queue_state.pending_queue_action = Some(action);
            self.queue_state.pending_delete = None;
            self.status = None;
            cx.notify();
            return;
        }
        self.queue_state.pending_queue_action = None;
        self.queue_state.pending_delete = None;
        self.busy = true;
        self.status = None;
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let queue_for_action = queue.clone();
            let result = cx
                .background_spawn(async move {
                    smol::unblock(move || match action {
                        QueueAction::Purge => client.purge_queue(&queue_for_action),
                        QueueAction::Delete => client.delete_queue(&queue_for_action),
                    })
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => match action {
                        QueueAction::Purge => {
                            this.queue_state.messages.clear();
                            this.queue_state.attributes = None;
                            this.status =
                                Some((rust_i18n::t!("aws.sqs.purged").to_string(), false));
                        }
                        QueueAction::Delete => {
                            this.queue_state.queues.retain(|url| url != &queue);
                            this.queue_state.selected = None;
                            this.queue_state.attributes = None;
                            this.queue_state.messages.clear();
                            this.status =
                                Some((rust_i18n::t!("aws.sqs.queue_deleted").to_string(), false));
                        }
                    },
                    Err(error) => this.status = Some((error.to_string(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for SqsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(sent_body) = self.clear_body.take()
            && self.body.read(cx).value() == sent_body
        {
            self.body
                .update(cx, |body, cx| body.set_value("", window, cx));
        }
        if let Some(sent_attributes) = self.clear_message_attributes.take()
            && self.message_attributes.read(cx).value() == sent_attributes
        {
            self.message_attributes
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some(value) = self.clear_queue_delay.take()
            && self.queue_delay.read(cx).value() == value
        {
            self.queue_delay
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some(value) = self.clear_queue_visibility_timeout.take()
            && self.queue_visibility_timeout.read(cx).value() == value
        {
            self.queue_visibility_timeout
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some(value) = self.clear_queue_receive_wait.take()
            && self.queue_receive_wait.read(cx).value() == value
        {
            self.queue_receive_wait
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some(value) = self.clear_queue_message_retention.take()
            && self.queue_message_retention.read(cx).value() == value
        {
            self.queue_message_retention
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let tokens = Tokens::global(cx).clone();
        let selected = self.queue_state.selected.clone();
        let pending_delete = self.queue_state.pending_delete.clone();
        let pending_queue_action = self.queue_state.pending_queue_action;
        let queue_filter = self.queue_filter.read(cx).value().trim().to_lowercase();
        let matching_queues = self
            .queue_state
            .queues
            .iter()
            .filter(|queue| queue.to_lowercase().contains(&queue_filter))
            .count();
        let is_fifo = selected
            .as_deref()
            .is_some_and(|url| url.ends_with(".fifo"));
        let show_queues = self.credentials.is_some() || self.console_connected;
        let queue_name = |url: &str| url.rsplit('/').next().unwrap_or(url).to_string();
        v_flex()
            .size_full()
            .text_color(tokens.colors().text_primary)
            .text_size(px(13.))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w(px(250.))
                            .h_full()
                            .flex_shrink_0()
                            .bg(tokens.colors().bg_sidebar)
                            .border_r_1()
                            .border_color(tokens.colors().border_subtle)
                            .child(
                                h_flex()
                                    .h(px(44.))
                                    .pl(px(76.))
                                    .pr_3()
                                    .items_center()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_size(px(14.))
                                    .child("AWS"),
                            )
                            .child(
                                div()
                                    .px_5()
                                    .pt_4()
                                    .pb_2()
                                    .text_size(px(11.))
                                    .text_color(tokens.colors().text_muted)
                                    .child(rust_i18n::t!("aws.services").to_string()),
                            )
                            .child(
                                h_flex()
                                    .mx_2()
                                    .px_2p5()
                                    .h(px(36.))
                                    .gap_2()
                                    .items_center()
                                    .rounded(px(tokens.radius.row))
                                    .bg(tokens.colors().row_active())
                                    .child(
                                        gpui_component::Icon::new(IconName::Inbox)
                                            .size_4()
                                            .text_color(tokens.colors().accent),
                                    )
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("SQS"),
                            )
                            .child(div().flex_1())
                            .when(show_queues, |rail| rail.child(
                                v_flex()
                                    .p_2()
                                    .border_t_1()
                                    .border_color(tokens.colors().border_subtle)
                                    .child(
                                        v_flex()
                                            .p_2()
                                            .gap_2()
                                            .rounded(px(tokens.radius.row))
                                            .hover(|panel| panel.bg(tokens.colors().row_hover()))
                                            .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                .child(rust_i18n::t!("aws.connection").to_string()))
                                            .when_some(self.selected_account.as_ref(), |panel, account| {
                                                panel.child(
                                                    v_flex()
                                                        .gap_0p5()
                                                        .child(div().font_weight(FontWeight::MEDIUM).truncate().child(account.account_name.clone()))
                                                        .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                            .child(account.account_id.clone())),
                                                )
                                            })
                                            .when(self.console_connected, |panel| panel.child(
                                                div().font_weight(FontWeight::MEDIUM)
                                                    .child(rust_i18n::t!("aws.console_login").to_string())
                                            ))
                                            .when_some(self.selected_role.as_ref(), |panel, role| {
                                                panel.child(div().text_size(px(11.)).text_color(tokens.colors().text_secondary)
                                                    .truncate().child(role.role_name.clone()))
                                            })
                                            .child(v_flex().gap_1().pt_2()
                                                .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                    .child(rust_i18n::t!("aws.region").to_string()))
                                                .child(Input::new(&self.region).disabled(self.login_busy || self.busy)))
                                            .child(Button::new("aws-change-login")
                                                .label(rust_i18n::t!("aws.change_login").to_string())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.auth_generation = this.auth_generation.wrapping_add(1);
                                                    this.session = None;
                                                    this.accounts.clear();
                                                    this.roles.clear();
                                                    this.selected_account = None;
                                                    this.selected_role = None;
                                                    this.credentials = None;
                                                    this.console_connected = false;
                                                    this.queues_loaded = false;
                                                    this.queue_state.clear();
                                                    this.login_feedback = None;
                                                    this.login_code = None;
                                                    this.login_url = None;
                                                    cx.notify();
                                                }))),
                                    ),
                            )),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(320.))
                            .h_full()
                            .border_r_1()
                            .border_color(tokens.colors().border_subtle)
                                    .when(!show_queues, |column| {
                                        column.child(
                                            v_flex()
                                                .w_full()
                                                .h_full()
                                                .items_center()
                                                .justify_center()
                                                .px_8()
                                                .child(
                                                    v_flex()
                                                        .w_full()
                                                        .max_w(px(420.))
                                                        .items_center()
                                                        .gap_5()
                                                        .child(
                                                            gpui_component::Icon::new(IconName::Inbox)
                                                                .size(px(56.))
                                                                .text_color(tokens.colors().accent),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(22.))
                                                                .font_weight(FontWeight::MEDIUM)
                                                                .child(rust_i18n::t!("aws.sign_in_title").to_string()),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_color(tokens.colors().text_secondary)
                                                                .text_center()
                                                                .child(if self.login_method == LoginMethod::Console {
                                                                    rust_i18n::t!("aws.console_hint").to_string()
                                                                } else {
                                                                    rust_i18n::t!("aws.sign_in_hint").to_string()
                                                                }),
                                                        )
                                                        .when(!self.show_connection_settings && self.session.is_none() && self.login_method == LoginMethod::IdentityCenter, |content| content.child(
                                                            div()
                                                                .text_color(tokens.colors().text_muted)
                                                                .text_center()
                                                                .child(format!("{} · {}", self.start_url.read(cx).value(), self.region.read(cx).value())),
                                                        ))
                                                        .when(self.show_connection_settings, |content| content.child(
                                                            v_flex()
                                                                .w_full()
                                                                .gap_3()
                                                                .child(h_flex().w_full().gap_2()
                                                                    .child(Button::new("aws-method-console")
                                                                        .label(rust_i18n::t!("aws.console_login").to_string())
                                                                        .when(self.login_method == LoginMethod::Console, |button| button.primary())
                                                                        .disabled(self.login_busy)
                                                                        .on_click(cx.listener(|this, _, _, cx| this.select_login_method(LoginMethod::Console, cx))))
                                                                    .child(Button::new("aws-method-identity-center")
                                                                        .label(rust_i18n::t!("aws.identity_center_login").to_string())
                                                                        .when(self.login_method == LoginMethod::IdentityCenter, |button| button.primary())
                                                                        .disabled(self.login_busy)
                                                                        .on_click(cx.listener(|this, _, _, cx| this.select_login_method(LoginMethod::IdentityCenter, cx)))))
                                                                .when(self.login_method == LoginMethod::Console, |form| form.child(
                                                                    v_flex().w_full().gap_2()
                                                                        .child(div().text_color(tokens.colors().text_secondary)
                                                                            .child(rust_i18n::t!("aws.console_requirements").to_string()))
                                                                        .child(v_flex().gap_1()
                                                                            .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                                                .child(rust_i18n::t!("aws.region").to_string()))
                                                                            .child(Input::new(&self.region).disabled(self.login_busy || self.busy)))
                                                                ))
                                                                .when(self.login_method == LoginMethod::IdentityCenter, |form| form.child(
                                                                    v_flex().w_full().gap_3().child(
                                                                    v_flex()
                                                                        .gap_1()
                                                                        .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                                            .child(rust_i18n::t!("aws.start_url").to_string()))
                                                                        .child(Input::new(&self.start_url).disabled(self.login_busy || self.busy)),
                                                                )
                                                                .child(
                                                                    h_flex()
                                                                        .w_full()
                                                                        .gap_3()
                                                                        .child(v_flex().flex_1().min_w_0().gap_1()
                                                                            .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                                                .child(rust_i18n::t!("aws.sso_region").to_string()))
                                                                            .child(Input::new(&self.sso_region).disabled(self.login_busy || self.busy)))
                                                                        .child(v_flex().flex_1().min_w_0().gap_1()
                                                                            .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                                                .child(rust_i18n::t!("aws.region").to_string()))
                                                                            .child(Input::new(&self.region).disabled(self.login_busy || self.busy))),
                                                                )
                                                                .when(self.session.is_none(), |form| form.child(
                                                                    v_flex()
                                                                        .w_full()
                                                                        .gap_2()
                                                                        .child(
                                                                            h_flex()
                                                                                .w_full()
                                                                                .gap_2()
                                                                                .p_1()
                                                                                .rounded(px(tokens.radius.panel))
                                                                                .bg(tokens.colors().bg_surface)
                                                                                .child(
                                                                                    Button::new("aws-login-pkce")
                                                                                        .label(rust_i18n::t!("aws.login_pkce").to_string())
                                                                                        .when(self.auth_flow == AuthFlow::Pkce, |button| button.primary())
                                                                                        .disabled(self.login_busy)
                                                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                                                            this.auth_flow = AuthFlow::Pkce;
                                                                                            cx.notify();
                                                                                        })),
                                                                                )
                                                                                .child(
                                                                                    Button::new("aws-login-device")
                                                                                        .label(rust_i18n::t!("aws.login_device").to_string())
                                                                                        .when(self.auth_flow == AuthFlow::Device, |button| button.primary())
                                                                                        .disabled(self.login_busy)
                                                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                                                            this.auth_flow = AuthFlow::Device;
                                                                                            cx.notify();
                                                                                        })),
                                                                                ),
                                                                        )
                                                                        .child(
                                                                            div()
                                                                                .text_color(tokens.colors().text_secondary)
                                                                                .child(match self.auth_flow {
                                                                                    AuthFlow::Pkce => rust_i18n::t!("aws.pkce_hint").to_string(),
                                                                                    AuthFlow::Device => rust_i18n::t!("aws.device_hint").to_string(),
                                                                                }),
                                                                        ),
                                                                )),
                                                                    )),
                                                        ))
                                                        .when(self.session.is_none(), |content| content.child(
                                                            v_flex()
                                                                .w_full()
                                                                .gap_2()
                                                                .child(
                                                                    Button::new("aws-login-main")
                                                                        .label(if self.login_method == LoginMethod::Console {
                                                                            rust_i18n::t!("aws.console_login_action").to_string()
                                                                        } else {
                                                                            match self.auth_flow {
                                                                                AuthFlow::Pkce => rust_i18n::t!("aws.login").to_string(),
                                                                                AuthFlow::Device => rust_i18n::t!("aws.login_device_action").to_string(),
                                                                            }
                                                                        })
                                                                        .primary()
                                                                        .disabled(self.login_busy)
                                                                        .on_click(cx.listener(|this, _, _, cx| this.login(cx))),
                                                                )
                                                                .child(
                                                                    Button::new("aws-connection-settings")
                                                                        .label(if self.show_connection_settings {
                                                                            rust_i18n::t!("aws.hide_connection_settings").to_string()
                                                                        } else {
                                                                            rust_i18n::t!("aws.connection_settings").to_string()
                                                                        })
                                                                        .disabled(self.login_busy)
                                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                                            this.show_connection_settings = !this.show_connection_settings;
                                                                            cx.notify();
                                                                        })),
                                                                ),
                                                        ))
                                                        .when(self.login_busy && self.login_code.is_none(), |content| {
                                                            content.child(
                                                                h_flex()
                                                                    .gap_2()
                                                                    .items_center()
                                                                    .text_color(tokens.colors().text_secondary)
                                                                    .child(
                                                                        gpui_component::spinner::Spinner::new()
                                                                            .icon(IconName::LoaderCircle)
                                                                            .with_size(px(12.))
                                                                            .color(tokens.colors().text_muted),
                                                                    )
                                                                    .child(rust_i18n::t!("aws.login_in_progress").to_string()),
                                                            )
                                                        })
                                                        .when_some(self.login_code.clone(), |content, code| {
                                                            content.child(
                                                                v_flex()
                                                                    .items_center()
                                                                    .gap_2()
                                                                    .child(
                                                                        div()
                                                                            .text_color(tokens.colors().text_secondary)
                                                                            .child(rust_i18n::t!("aws.device_prompt").to_string()),
                                                                    )
                                                                    .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).child(code.clone()))
                                                                    .child(
                                                                        Button::new("aws-copy-login-code-main")
                                                                            .label(rust_i18n::t!("aws.copy_code").to_string())
                                                                            .on_click(move |_, _, cx| {
                                                                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                                                            }),
                                                                    ),
                                                            )
                                                        })
                                                        .when_some(self.login_url.clone(), |content, url| {
                                                            content.child(
                                                                Button::new("aws-open-login-url-main")
                                                                    .label(rust_i18n::t!("aws.open_login_url").to_string())
                                                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                                                            )
                                                        })
                                                        .when_some(self.login_feedback.as_ref().filter(|_| !self.login_busy), |content, (message, error)| {
                                                            content.child(
                                                                div()
                                                                    .text_center()
                                                                    .text_color(if *error { tokens.colors().status_error } else { tokens.colors().text_secondary })
                                                                    .child(message.clone()),
                                                            )
                                                        })
                                                        .when(!self.accounts.is_empty(), |content| content.child(
                                                            v_flex().w_full().gap_2()
                                                                .child(rust_i18n::t!("aws.account").to_string())
                                                                .children(self.accounts.iter().map(|account| {
                                                                    let chosen = account.clone();
                                                                    Button::new(format!("aws-account-{}", account.account_id))
                                                                        .label(format!("{} ({})", account.account_name, account.account_id))
                                                                        .disabled(self.login_busy)
                                                                        .on_click(cx.listener(move |this, _, _, cx| this.select_account(chosen.clone(), cx)))
                                                                })),
                                                        ))
                                                        .when(!self.roles.is_empty(), |content| content.child(
                                                            v_flex().w_full().gap_2()
                                                                .child(rust_i18n::t!("aws.role").to_string())
                                                                .children(self.roles.iter().map(|role| {
                                                                    let chosen = role.clone();
                                                                    Button::new(format!("aws-role-{}", role.role_name))
                                                                        .label(role.role_name.clone())
                                                                        .disabled(self.login_busy)
                                                                        .on_click(cx.listener(move |this, _, _, cx| this.select_role(chosen.clone(), cx)))
                                                                })),
                                                        ))
                                                        .when(show_queues, |content| content.child(
                                                            v_flex()
                                                                .w_full()
                                                                .pt_5()
                                                                .gap_2()
                                                                .border_t_1()
                                                                .border_color(tokens.colors().border_subtle)
                                                                .child(Input::new(&self.queue_url))
                                                                .child(
                                                                    Button::new("aws-open-queue-url-sign-in")
                                                                        .label(rust_i18n::t!("aws.sqs.open_queue_url").to_string())
                                                                        .disabled(self.busy)
                                                                        .on_click(cx.listener(|this, _, _, cx| this.open_queue_url(cx))),
                                                                ),
                                                        )),
                                                ),
                                        )
                                    })
                            .when(show_queues, |column| column.child(
                                v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .h_full()
                                    .child(
                                h_flex()
                                    .h(px(44.))
                                    .px_4()
                                    .items_center()
                                    .border_b_1()
                                    .border_color(tokens.colors().border_subtle)
                                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(rust_i18n::t!("aws.sqs.queues").to_string()))
                                    .child(
                                        Button::new("aws-refresh")
                                            .label(rust_i18n::t!("aws.refresh").to_string())
                                            .disabled(self.busy)
                                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .px_3()
                                    .py_3()
                                    .child(Input::new(&self.queue_filter)),
                            )
                            .child(
                                v_flex()
                                    .id("aws-queues")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .px_2()
                                    .gap_1()
                                    .when(
                                        self.queue_state.queues.is_empty()
                                            && !self.busy
                                            && self.status.is_none(),
                                        |list| {
                                            list.child(
                                                div()
                                                    .px_2()
                                                    .py_3()
                                                    .text_color(tokens.colors().text_muted)
                                                    .child(if self.needs_refresh {
                                                        rust_i18n::t!(
                                                            "aws.sqs.refresh_for_identity"
                                                        )
                                                        .to_string()
                                                    } else {
                                                        rust_i18n::t!("aws.sqs.no_queues")
                                                            .to_string()
                                                    }),
                                            )
                                        },
                                    )
                                    .when(
                                        matching_queues == 0 && !self.queue_state.queues.is_empty(),
                                        |list| {
                                            list.child(
                                                div()
                                                    .px_2()
                                                    .py_3()
                                                    .text_color(tokens.colors().text_muted)
                                                    .child(
                                                        rust_i18n::t!("aws.sqs.no_match")
                                                            .to_string(),
                                                    ),
                                            )
                                        },
                                    )
                                    .children(
                                        self.queue_state
                                            .queues
                                            .iter()
                                            .filter(|queue| {
                                                queue.to_lowercase().contains(&queue_filter)
                                            })
                                            .map(|queue| {
                                                let chosen =
                                                    selected.as_deref() == Some(queue.as_str());
                                                let url = queue.clone();
                                                h_flex()
                                                    .id(format!("aws-queue-{queue}"))
                                                    .w_full()
                                                    .h(px(34.))
                                                    .px_3()
                                                    .items_center()
                                                    .rounded(px(tokens.radius.row))
                                                    .cursor_pointer()
                                                    .when(chosen, |row| row.bg(tokens.colors().row_active()))
                                                    .hover(|row| row.bg(tokens.colors().row_hover()))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.select(url.clone(), cx)
                                                    }))
                                                    .child(div().truncate().child(queue_name(queue)))
                                            }),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .p_3()
                                    .gap_3()
                                    .border_t_1()
                                    .border_color(tokens.colors().border_subtle)
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                .child(rust_i18n::t!("aws.sqs.create_queue").to_string()))
                                            .child(
                                                h_flex().gap_2()
                                                    .child(div().flex_1().min_w_0()
                                                        .child(Input::new(&self.new_queue_name).disabled(self.busy)))
                                                    .child(Button::new("aws-create-queue")
                                                        .label(rust_i18n::t!("aws.sqs.create_queue").to_string())
                                                        .disabled(self.busy)
                                                        .on_click(cx.listener(|this, _, _, cx| this.create_queue(cx)))),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_size(px(11.)).text_color(tokens.colors().text_muted)
                                                .child(rust_i18n::t!("aws.sqs.queue_url_placeholder").to_string()))
                                            .child(
                                                h_flex().gap_2()
                                                    .child(div().flex_1().min_w_0().child(Input::new(&self.queue_url)))
                                                    .child(Button::new("aws-open-queue-url")
                                                        .label(rust_i18n::t!("aws.sqs.open_queue_url").to_string())
                                                        .disabled(self.busy)
                                                        .on_click(cx.listener(|this, _, _, cx| this.open_queue_url(cx)))),
                                            ),
                                    ),
                            ),
                                    ))
                    )
                    .when(show_queues, |main| main.child(
                        v_flex()
                            .w(px(500.))
                            .flex_shrink_0()
                            .h_full()
                            .border_l_1()
                            .border_color(tokens.colors().border_subtle)
                            .child(
                                h_flex()
                                    .h(px(44.))
                                    .px_5()
                                    .gap_3()
                                    .items_center()
                                    .border_b_1()
                                    .border_color(tokens.colors().border_subtle)
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(
                                                selected.as_deref().map(queue_name).unwrap_or_else(
                                                    || {
                                                        rust_i18n::t!("aws.sqs.select_queue")
                                                            .to_string()
                                                    },
                                                ),
                                            ),
                                    )
                                    .when_some(selected.clone(), |row, queue| {
                                        row.child(
                                            Button::new("aws-attributes")
                                                .label(
                                                    rust_i18n::t!("aws.sqs.refresh_attributes")
                                                        .to_string(),
                                                )
                                                .disabled(self.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.refresh_attributes(queue.clone(), cx)
                                                })),
                                        )
                                    }),
                            )
                            .child(
                                v_flex()
                                    .id("aws-detail")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .px_5()
                                    .py_4()
                                    .gap_4()
                                    .when(self.queues_loaded && selected.is_none(), |detail| {
                                        detail.child(
                                            v_flex()
                                                .w_full()
                                                .h_full()
                                                .items_center()
                                                .justify_center()
                                                .gap_2()
                                                .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child(rust_i18n::t!("aws.sqs.select_queue").to_string()))
                                                .child(div().text_color(tokens.colors().text_muted).child(rust_i18n::t!("aws.sqs.select_queue_hint").to_string())),
                                        )
                                    })
                                    .when_some(selected.clone(), |detail, queue| {
                                        detail
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(tokens.colors().text_muted)
                                                    .child(queue),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(tokens.colors().text_muted)
                                                    .child(rust_i18n::t!("aws.sqs.overview").to_string()),
                                            )
                                            .when_some(
                                                self.queue_state.attributes.as_ref(),
                                                |detail, attrs| {
                                                    detail.child(
                                                        h_flex()
                                                            .gap_5()
                                                            .p_3()
                                                            .rounded(px(tokens.radius.card))
                                                            .bg(tokens.colors().bg_surface)
                                                            .child(format!(
                                                                "{}: {}",
                                                                rust_i18n::t!("aws.sqs.available"),
                                                                attrs.available
                                                            ))
                                                            .child(format!(
                                                                "{}: {}",
                                                                rust_i18n::t!("aws.sqs.in_flight"),
                                                                attrs.in_flight
                                                            ))
                                                            .child(format!(
                                                                "{}: {}",
                                                                rust_i18n::t!("aws.sqs.delayed"),
                                                                attrs.delayed
                                                            )),
                                                    )
                                                    .when_some(attrs.delay_seconds, |detail, seconds| {
                                                        detail.child(format!(
                                                            "{}: {} s",
                                                            rust_i18n::t!("aws.sqs.queue_delay_current"),
                                                            seconds
                                                        ))
                                                    })
                                                    .when_some(attrs.visibility_timeout, |detail, seconds| {
                                                        detail.child(format!(
                                                            "{}: {} s",
                                                            rust_i18n::t!("aws.sqs.queue_visibility_current"),
                                                            seconds
                                                        ))
                                                    })
                                                    .when_some(attrs.receive_wait_seconds, |detail, seconds| {
                                                        detail.child(format!(
                                                            "{}: {} s",
                                                            rust_i18n::t!("aws.sqs.queue_receive_wait_current"),
                                                            seconds
                                                        ))
                                                    })
                                                    .when_some(attrs.message_retention_period, |detail, seconds| {
                                                        detail.child(format!(
                                                            "{}: {} s",
                                                            rust_i18n::t!("aws.sqs.queue_message_retention_current"),
                                                            seconds
                                                        ))
                                                    })
                                                },
                                            )
                                            .child(
                                                div()
                                                    .pt_3()
                                                    .border_t_1()
                                                    .border_color(tokens.colors().border_subtle)
                                                    .text_size(px(11.))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(tokens.colors().text_muted)
                                                    .child(rust_i18n::t!("aws.sqs.queue_settings").to_string()),
                                            )
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_3()
                                                    .child(
                                                        div()
                                                            .w(px(210.))
                                                            .child(Input::new(&self.queue_delay).disabled(self.busy)),
                                                    )
                                                    .child(
                                                        Button::new("aws-apply-queue-delay")
                                                            .label(rust_i18n::t!("aws.sqs.apply_queue_delay").to_string())
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(|this, _, _, cx| this.apply_queue_delay(cx))),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_3()
                                                    .child(
                                                        div()
                                                            .w(px(210.))
                                                            .child(Input::new(&self.queue_visibility_timeout).disabled(self.busy)),
                                                    )
                                                    .child(
                                                        Button::new("aws-apply-queue-visibility")
                                                            .label(rust_i18n::t!("aws.sqs.apply_queue_visibility").to_string())
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(|this, _, _, cx| this.apply_queue_visibility_timeout(cx))),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_3()
                                                    .child(
                                                        div()
                                                            .w(px(210.))
                                                            .child(Input::new(&self.queue_receive_wait).disabled(self.busy)),
                                                    )
                                                    .child(
                                                        Button::new("aws-apply-queue-receive-wait")
                                                            .label(rust_i18n::t!("aws.sqs.apply_queue_receive_wait").to_string())
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(|this, _, _, cx| this.apply_queue_receive_wait(cx))),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_3()
                                                    .child(
                                                        div()
                                                            .w(px(210.))
                                                            .child(Input::new(&self.queue_message_retention).disabled(self.busy)),
                                                    )
                                                    .child(
                                                        Button::new("aws-apply-queue-message-retention")
                                                            .label(if self.queue_state.pending_message_retention.is_some() {
                                                                rust_i18n::t!("aws.sqs.confirm_queue_message_retention").to_string()
                                                            } else {
                                                                rust_i18n::t!("aws.sqs.apply_queue_message_retention").to_string()
                                                            })
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(|this, _, _, cx| this.apply_queue_message_retention(cx))),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_color(tokens.colors().text_muted)
                                                    .child(rust_i18n::t!("aws.sqs.queue_message_retention_warning").to_string()),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Button::new("aws-purge-queue")
                                                            .label(if pending_queue_action
                                                                == Some(QueueAction::Purge)
                                                            {
                                                                rust_i18n::t!(
                                                                    "aws.sqs.confirm_purge_queue"
                                                                )
                                                                .to_string()
                                                            } else {
                                                                rust_i18n::t!("aws.sqs.purge_queue")
                                                                    .to_string()
                                                            })
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(|this, _, _, cx| {
                                                                this.queue_action(
                                                                    QueueAction::Purge,
                                                                    cx,
                                                                )
                                                            })),
                                                    )
                                                    .child(
                                                        Button::new("aws-delete-queue")
                                                            .label(if pending_queue_action
                                                                == Some(QueueAction::Delete)
                                                            {
                                                                rust_i18n::t!(
                                                                    "aws.sqs.confirm_delete_queue"
                                                                )
                                                                .to_string()
                                                            } else {
                                                                rust_i18n::t!("aws.sqs.delete_queue")
                                                                    .to_string()
                                                            })
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(|this, _, _, cx| {
                                                                this.queue_action(
                                                                    QueueAction::Delete,
                                                                    cx,
                                                                )
                                                            })),
                                                    )
                                                    .when(pending_queue_action.is_some(), |row| {
                                                        row.child(
                                                            Button::new("aws-cancel-queue-action")
                                                                .label(
                                                                    rust_i18n::t!(
                                                                        "aws.sqs.cancel_queue_action"
                                                                    )
                                                                    .to_string(),
                                                                )
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        this.queue_state
                                                                            .pending_queue_action =
                                                                            None;
                                                                        cx.notify();
                                                                    },
                                                                )),
                                                        )
                                                    }),
                                            )
                                            .when(pending_queue_action.is_some(), |detail| {
                                                detail.child(
                                                    div()
                                                        .text_color(tokens.colors().text_muted)
                                                        .child(
                                                            rust_i18n::t!(
                                                                "aws.sqs.queue_action_warning"
                                                            )
                                                            .to_string(),
                                                        ),
                                                )
                                            })
                                            .child(
                                                v_flex()
                                                    .pt_3()
                                                    .gap_2()
                                                    .border_t_1()
                                                    .border_color(tokens.colors().border_subtle)
                                                    .child(
                                                        div()
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .child(
                                                                rust_i18n::t!("aws.sqs.send")
                                                                    .to_string(),
                                                            ),
                                                    )
                                                    .child(Textarea::new(&self.body))
                                                    .child(
                                                        div().child(
                                                            rust_i18n::t!("aws.sqs.message_attributes_label")
                                                                .to_string(),
                                                        ),
                                                    )
                                                    .child(Textarea::new(&self.message_attributes))
                                                    .when(!is_fifo, |form| {
                                                        form.child(
                                                            div().child(
                                                                rust_i18n::t!("aws.sqs.send_delay_label")
                                                                    .to_string(),
                                                            ),
                                                        )
                                                        .child(Input::new(&self.send_delay))
                                                    })
                                                    .when(is_fifo, |form| {
                                                        form.child(
                                                            div().child(
                                                                rust_i18n::t!("aws.sqs.group_id")
                                                                    .to_string(),
                                                            ),
                                                        )
                                                        .child(Input::new(&self.group_id))
                                                        .child(
                                                            div().child(
                                                                rust_i18n::t!(
                                                                    "aws.sqs.deduplication_id"
                                                                )
                                                                .to_string(),
                                                            ),
                                                        )
                                                        .child(Input::new(&self.deduplication_id))
                                                    })
                                                    .child(
                                                        Button::new("aws-send")
                                                            .label(
                                                                rust_i18n::t!("aws.sqs.send")
                                                                    .to_string(),
                                                            )
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| this.send(cx),
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .pt_3()
                                                    .border_t_1()
                                                    .border_color(tokens.colors().border_subtle)
                                                    .items_center()
                                                    .gap_3()
                                                    .child(
                                                        div()
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .child(
                                                                rust_i18n::t!("aws.sqs.messages")
                                                                    .to_string(),
                                                            ),
                                                    )
                                                    .child(
                                                        Button::new("aws-receive-wait")
                                                            .label(match self.receive_wait {
                                                                ReceiveWait::Short => rust_i18n::t!("aws.sqs.receive_wait_short").to_string(),
                                                                ReceiveWait::Long => rust_i18n::t!("aws.sqs.receive_wait_long").to_string(),
                                                                ReceiveWait::QueueDefault => rust_i18n::t!("aws.sqs.receive_wait_queue_default").to_string(),
                                                            })
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.receive_wait =
                                                                        this.receive_wait.next();
                                                                    cx.notify();
                                                                },
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .items_end()
                                                    .gap_3()
                                                    .child(
                                                        v_flex()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_size(px(11.))
                                                                    .child(
                                                                        rust_i18n::t!("aws.sqs.receive_count_label")
                                                                            .to_string(),
                                                                    ),
                                                            )
                                                            .child(
                                                                div().w(px(72.)).child(
                                                                    Input::new(&self.receive_count)
                                                                        .disabled(self.busy),
                                                                ),
                                                            ),
                                                    )
                                                    .child(
                                                        v_flex()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_size(px(11.))
                                                                    .child(
                                                                        rust_i18n::t!("aws.sqs.visibility_timeout_label")
                                                                            .to_string(),
                                                                    ),
                                                            )
                                                            .child(
                                                                div().w(px(150.)).child(
                                                                    Input::new(
                                                                        &self.receive_visibility_timeout,
                                                                    )
                                                                    .disabled(self.busy),
                                                                ),
                                                            ),
                                                    )
                                                    .child(
                                                        Button::new("aws-receive")
                                                            .label(
                                                                rust_i18n::t!("aws.sqs.receive")
                                                                    .to_string(),
                                                            )
                                                            .disabled(self.busy)
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| this.receive(cx),
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_3()
                                                    .child(
                                                        div()
                                                            .text_size(px(11.))
                                                            .child(
                                                                rust_i18n::t!("aws.sqs.message_visibility_label")
                                                                    .to_string(),
                                                            ),
                                                    )
                                                    .child(
                                                        div().w(px(150.)).child(
                                                            Input::new(&self.message_visibility_timeout)
                                                                .disabled(self.busy),
                                                        ),
                                                    ),
                                            )
                                            .children(self.queue_state.messages.iter().map(
                                                |message| {
                                                    let receipt = message.receipt_handle.clone();
                                                    let confirm = pending_delete.as_deref()
                                                        == Some(receipt.as_str());
                                                    let sent_at = message
                                                        .sent_at_millis
                                                        .and_then(|millis| i64::try_from(millis).ok())
                                                        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                                                        .map(|time| {
                                                            time.with_timezone(&chrono::Local)
                                                                .format("%Y-%m-%d %H:%M:%S %Z")
                                                                .to_string()
                                                        });
                                                    v_flex()
                                                        .p_3()
                                                        .gap_2()
                                                        .rounded(px(tokens.radius.card))
                                                        .bg(tokens.colors().bg_surface)
                                                        .child(
                                                            h_flex()
                                                                .items_center()
                                                                .gap_3()
                                                                .child(
                                                                    div()
                                                                        .flex_1()
                                                                        .text_size(px(12.))
                                                                        .text_color(
                                                                            tokens
                                                                                .colors()
                                                                                .text_muted,
                                                                        )
                                                                        .child(message.id.clone()),
                                                                )
                                                                .child(
                                                                    Button::new(format!(
                                                                        "aws-visibility-{}",
                                                                        message.id
                                                                    ))
                                                                    .label(
                                                                        rust_i18n::t!(
                                                                            "aws.sqs.apply_visibility"
                                                                        )
                                                                        .to_string(),
                                                                    )
                                                                    .disabled(self.busy)
                                                                    .on_click(cx.listener({
                                                                        let receipt =
                                                                            receipt.clone();
                                                                        move |this, _, _, cx| {
                                                                            this.apply_message_visibility(
                                                                                receipt.clone(),
                                                                                cx,
                                                                            )
                                                                        }
                                                                    })),
                                                                )
                                                                .child(
                                                                    Button::new(format!(
                                                                        "aws-release-{}",
                                                                        message.id
                                                                    ))
                                                                    .label(
                                                                        rust_i18n::t!(
                                                                            "aws.sqs.release"
                                                                        )
                                                                        .to_string(),
                                                                    )
                                                                    .disabled(self.busy)
                                                                    .on_click(cx.listener({
                                                                        let receipt =
                                                                            receipt.clone();
                                                                        move |this, _, _, cx| {
                                                                            this.change_message_visibility(
                                                                                receipt.clone(),
                                                                                0,
                                                                                cx,
                                                                            )
                                                                        }
                                                                    })),
                                                                )
                                                                .child(
                                                                    Button::new(format!(
                                                                        "aws-delete-{}",
                                                                        message.id
                                                                    ))
                                                                    .label(if confirm {
                                                                        rust_i18n::t!(
                                                                            "aws.sqs.confirm_delete"
                                                                        )
                                                                        .to_string()
                                                                    } else {
                                                                        rust_i18n::t!(
                                                                            "aws.sqs.delete"
                                                                        )
                                                                        .to_string()
                                                                    })
                                                                    .disabled(self.busy)
                                                                    .on_click(cx.listener(
                                                                        move |this, _, _, cx| {
                                                                            this.delete(
                                                                                receipt.clone(),
                                                                                cx,
                                                                            )
                                                                        },
                                                                    )),
                                                                ),
                                                        )
                                                        .child(
                                                            v_flex()
                                                                .gap_1()
                                                                .text_size(px(11.))
                                                                .text_color(tokens.colors().text_muted)
                                                                .when_some(message.receive_count, |meta, count| {
                                                                    meta.child(format!(
                                                                        "{}: {count}",
                                                                        rust_i18n::t!("aws.sqs.receive_count")
                                                                    ))
                                                                })
                                                                .when_some(sent_at, |meta, time| {
                                                                    meta.child(format!(
                                                                        "{}: {time}",
                                                                        rust_i18n::t!("aws.sqs.sent_at")
                                                                    ))
                                                                })
                                                                .when_some(message.group_id.as_ref(), |meta, group| {
                                                                    meta.child(format!(
                                                                        "{}: {group}",
                                                                        rust_i18n::t!("aws.sqs.received_group_id")
                                                                    ))
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(13.))
                                                                .child(message.body.clone()),
                                                        )
                                                        .when(!message.message_attributes.is_empty(), |card| {
                                                            card.child(
                                                                v_flex()
                                                                    .gap_1()
                                                                    .text_size(px(11.))
                                                                    .text_color(tokens.colors().text_muted)
                                                                    .child(rust_i18n::t!("aws.sqs.message_attributes_received").to_string())
                                                                    .children(message.message_attributes.iter().map(|(name, attribute)| {
                                                                        let value = attribute.string_value.as_deref()
                                                                            .or(attribute.binary_value.as_deref())
                                                                            .unwrap_or("");
                                                                        div().child(format!("{name} ({}): {value}", attribute.data_type))
                                                                    })),
                                                            )
                                                        })
                                                },
                                            ))
                                    }),
                            ),
                    ))
            )
            .when_some(show_queues.then_some(self.status.as_ref()).flatten(), |root, (status, error)| {
                root.child(
                    div()
                        .px_5()
                        .py_2()
                        .border_t_1()
                        .border_color(tokens.colors().border_subtle)
                        .text_color(if *error {
                            tokens.colors().status_error
                        } else {
                            tokens.colors().text_secondary
                        })
                        .child(status.clone()),
                )
            })
    }
}
