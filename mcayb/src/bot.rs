use std::{future, io::{self, ErrorKind}, str::FromStr, sync::Arc, time::{Duration, SystemTime}};
use std::cmp::{Ordering, PartialEq};
use std::collections::HashMap;
use std::mem::replace;
use std::ops::Deref;
use std::path::PathBuf;

use allocvec::AllocVec;
use anyhow::{anyhow, bail, Result};
use ende::{Decode, Encode};
use once_cell::sync::Lazy;
use parse_display::Display;
use rand::{Rng, SeedableRng, thread_rng};
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use serenity::{
    all::{
        ActionRowComponent, ButtonStyle, CacheHttp, CommandInteraction, Context, CreateActionRow, CreateButton, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateInputText, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, CreateModal, EditMessage, EventHandler, GatewayIntents, InputTextStyle, Interaction, Message, Ready
    },
    async_trait, Client,
};
use serenity::all::{Cache, ChannelId, CreateAttachment, CreatePoll, CreatePollAnswer, GuildId, Http, MessageId, MessagePollVoteAddEvent};
use serenity::futures::StreamExt;
use serenity_commands::Commands;
use tokio::join;
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::{LocalSet, spawn_local, yield_now};
use url::{Host, Url};

use yapper::{base64_decode, base64_encode, DelOnDrop, dispatch_debug, escape_discord, ModInfo, NetCommand, Notification, pretty_status, reserved_mod_id, Response, ServerCommand, ServerStatus, Status};
use yapper::conf::Config;

use crate::{comm::send_command, conf::MCAYB, process_mods};
use crate::conf::{OngoingPoll, PollKey, PollKind, VERSION};

const TOKEN: &str = include_str!("../discord.token");

struct ProcessedMenu {
    menu: Menu,
    current: MenuHistory,
    back: bool,
    refresh: bool,
}

impl ProcessedMenu {
    pub fn message(self) -> CreateMessage {
        let mut buttons = self.menu.1;
        if self.refresh {
            buttons.insert(0, CreateButton::new(self.current.to_id()).label("Refresh").emoji('🔄').style(ButtonStyle::Primary))
        }
        if self.back && let Some(previous) = self.current.exit_page() {
            buttons.insert(0, CreateButton::new(previous.to_id()).label("Back").style(ButtonStyle::Secondary))
        }


        let response = CreateMessage::new()
            .embed(self.menu.0);

        let mut components: Vec<CreateActionRow> = Vec::new();
        let mut buttons_split: Vec<CreateButton> = Vec::new();
        for (i, button) in buttons.into_iter().enumerate() {
            buttons_split.push(button);

            if (i + 1) % 5 == 0 && i != 0 {
                components.push(CreateActionRow::Buttons(buttons_split));
                buttons_split = Vec::new();
            }
        }
        if !buttons_split.is_empty() {
            components.push(CreateActionRow::Buttons(buttons_split));
        }

        response.components(components)
    }

    #[allow(unused)]
    pub fn edit_message(self) -> EditMessage {
        let mut buttons = self.menu.1;
        if self.refresh {
            buttons.insert(0, CreateButton::new(self.current.to_id()).label("Refresh").emoji('🔄').style(ButtonStyle::Primary))
        }
        if self.back && let Some(previous) = self.current.exit_page() {
            buttons.insert(0, CreateButton::new(previous.to_id()).label("Back").style(ButtonStyle::Secondary))
        }


        let response = EditMessage::new()
            .embed(self.menu.0);

        let mut components: Vec<CreateActionRow> = Vec::new();
        let mut buttons_split: Vec<CreateButton> = Vec::new();
        for (i, button) in buttons.into_iter().enumerate() {
            buttons_split.push(button);

            if (i + 1) % 5 == 0 && i != 0 {
                components.push(CreateActionRow::Buttons(buttons_split));
                buttons_split = Vec::new();
            }
        }
        if !buttons_split.is_empty() {
            components.push(CreateActionRow::Buttons(buttons_split));
        }

        response.components(components)
    }

    pub fn interaction(self) -> CreateInteractionResponseMessage {
        let mut buttons = self.menu.1;
        if self.refresh {
            buttons.insert(0, CreateButton::new(self.current.to_id()).label("Refresh").emoji('🔄').style(ButtonStyle::Primary))
        }
        if self.back && let Some(previous) = self.current.exit_page() {
            buttons.insert(0, CreateButton::new(previous.to_id()).label("Back").style(ButtonStyle::Secondary))
        }


        let response = CreateInteractionResponseMessage::new()
            .embed(self.menu.0);

        // let mut button_data = Vec::new();
        let mut components: Vec<CreateActionRow> = Vec::new();
        let mut buttons_split: Vec<CreateButton> = Vec::new();
        for (i, button) in buttons.into_iter().enumerate() {
            // let button: Button = unsafe { mem::transmute(button) };
            // let ButtonKind::NonLink { custom_id, .. } = &button.data else { panic!() };
            // if button_data.contains(custom_id) {
            //     let menu = MenuHistory::from_id(custom_id);
            //     println!("Duplicated menu link {menu:?}");
            // }
            // button_data.push(custom_id.clone());
            // let button: CreateButton = unsafe { mem::transmute(button) };
            
            buttons_split.push(button);

            if (i + 1) % 5 == 0 && i != 0 {
                components.push(CreateActionRow::Buttons(buttons_split));
                buttons_split = Vec::new();
            }
        }
        if !buttons_split.is_empty() {
            components.push(CreateActionRow::Buttons(buttons_split));
        }

        response.components(components)
    }
}

struct Menu(CreateEmbed, Vec<CreateButton>);

impl From<CreateEmbed> for Menu {
    fn from(value: CreateEmbed) -> Self {
        Self(value, Vec::new())
    }
}

impl Menu {
    pub fn new(color: (u8, u8, u8), title: String, description: Option<String>) -> Self {
        let mut this = Self(CreateEmbed::new().color(color).title(title), Vec::new());
        if let Some(description) = description {
            this.0 = this.0.description(description);
        }
        this
    }

    pub fn from_embed(embed: CreateEmbed) -> Self {
        Self(embed, Vec::new())
    }

    pub fn fields(mut self, fields: Vec<(String, String, bool)>) -> Self {
        self.0 = self.0.fields(fields);
        self
    }

    pub fn logo(mut self, logo: &str) -> Self {
        self.0 = self.0.image(logo);
        self
    }

    pub fn footer(mut self, text: &str) -> Self {
        self.0 = self.0.footer(CreateEmbedFooter::new(text));
        self
    }

    pub fn author(mut self, author: &str) -> Self {
        self.0 = self.0.author(CreateEmbedAuthor::new(author));
        self
    }

    pub fn url(mut self, url: &str) -> Self {
        self.0 = self.0.url(url);
        self
    }

    pub fn buttons(mut self, buttons: Vec<CreateButton>) -> Self {
        if buttons.is_empty() {
            return self;
        }
        self.1 = buttons;
        self
    }

    pub fn build(self, current: &MenuHistory, back: bool, refresh: bool) -> ProcessedMenu {
        ProcessedMenu {
            menu: self,
            current: current.clone(),
            back,
            refresh
        }
    }
}

async fn send_err(h: &MenuHistory, error: &anyhow::Error) -> ProcessedMenu {
    dispatch_debug(error);

    let mut content = format!("WHOOPS! An error occurred: {:?}", error);

    if let Some(error) = error.downcast_ref::<io::Error>() {
        match error.kind() {
            kind @ ErrorKind::TimedOut
            | kind @ ErrorKind::ConnectionRefused
            | kind @ ErrorKind::ConnectionAborted
            | kind @ ErrorKind::ConnectionReset => {
                content = format!("Couldn't connect to server: {}", kind)
            }
            kind => content = format!("Server communication error: {}", kind),
        }
    }

    result_menu(h, false, &content).await
}

async fn send_unknown(h: &MenuHistory, resp: &Response) -> ProcessedMenu {
    dispatch_debug(anyhow!("Unexpected response: {resp}"));

    result_menu(h, false, "Server sent an invalid response. This is a bug").await
}

async fn unknown_server(h: &MenuHistory, server: &str) -> ProcessedMenu {
    result_menu(h, false, &format!("Unknown server: {:?}", server)).await
}

#[derive(Debug, Commands)]
enum AllCommands {
    /// Shows a dashboard where you can easily control all servers
    Dashboard,
    /// Lists registered minecraft servers
    Servers,
    /// Shows the current status of a server
    Status {
        /// The server name
        server: String,
    },
    /// Starts a server
    Start {
        /// The server name
        server: String,
    },
    /// Stops a server
    Stop {
        /// The server name
        server: String,
    },
    /// Reboots a server
    Reboot {
        /// The server name
        server: String,
    },
    /// Sends a console command to the server
    Command {
        /// The server name
        server: String,
        /// The command to send
        command: String,
    },
    /// Shows a menu of all the mods installed for a specific server
    Mods {
        /// The server name
        server: String
    },
    /// Registers this channel as a receiver for server updates
    UpdateMe,
    /// Test
    Test,
    /// DebugEndPoll
    DebugEndPoll{
        /// server
        server: String,
        /// mod_id
        mod_id: String,
    },
}

impl AllCommands {
    async fn run(self, shared: &Shared, interaction: &CommandInteraction) -> CreateInteractionResponseMessage {
        let null_menu = MenuHistory::new("null");
        match self {
            AllCommands::Dashboard => dashboard_menu(shared, &MenuHistory::new("dashboard")).await.interaction(),
            AllCommands::Servers => match send_command(shared, NetCommand::ListServers) {
                Ok(Response::List(statuses)) => {
                    let mut string = String::new();

                    for status in statuses {
                        string.push_str(&format!(
                            "Server: {:?}, Status: {:?}, Path: {:?}\n",
                            status.name, status.status, status.path
                        ));
                    }

                    CreateInteractionResponseMessage::new().content(string)
                }
                Ok(any) => send_unknown(&null_menu, &any).await.interaction(),
                Err(any) => send_err(&null_menu, &any).await.interaction(),
            },
            AllCommands::Mods { server } => {
                let h = MenuHistory {
                    history: vec![],
                    current: MenuUrl::page("mods", &[&server])
                };
                mod_menu(shared, &h, &server, 0).await.interaction()
            },
            AllCommands::Status { server } => match send_command(shared, NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Status,
            )) {
                Ok(Response::Status(status)) => result_menu(&null_menu, true, &format!("Server is {}", pretty_status(status.status))).await.interaction(),
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await.interaction(),
                Ok(any) => send_unknown(&null_menu, &any).await.interaction(),
                Err(any) => send_err(&null_menu, &any).await.interaction(),
            },
            AllCommands::Start { server } => match send_command(shared, NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Start,
            )) {
                Ok(Response::Ok) => result_menu(&null_menu, true, "Server started!").await.interaction(),
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await.interaction(),
                Ok(any) => send_unknown(&null_menu, &any).await.interaction(),
                Err(any) => send_err(&null_menu, &any).await.interaction(),
            },
            AllCommands::Stop { server } => match send_command(shared, NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Stop,
            )) {
                Ok(Response::Ok) => result_menu(&null_menu, true, "Server stopped.").await.interaction(),
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await.interaction(),
                Ok(any) => send_unknown(&null_menu, &any).await.interaction(),
                Err(any) => send_err(&null_menu, &any).await.interaction(),
            },
            AllCommands::Reboot { server } => match send_command(shared, NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Reboot,
            )) {
                Ok(Response::Ok) => result_menu(&null_menu, true, "Server is rebooting!").await.interaction(),
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await.interaction(),
                Ok(any) => send_unknown(&null_menu, &any).await.interaction(),
                Err(any) => send_err(&null_menu, &any).await.interaction(),
            },
            AllCommands::Command { server, command } => match send_command(shared, 
                NetCommand::ServerCommand(server.clone(), ServerCommand::Console(command.clone())),
            ) {
                Ok(Response::CommandOutput(output)) => CreateInteractionResponseMessage::new().content(format!(
                        "`/{}` => `{}`",
                        escape_discord(&command),
                        escape_discord(&output)
                    )),
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await.interaction(),
                Ok(any) => send_unknown(&null_menu, &any).await.interaction(),
                Err(any) => send_err(&null_menu, &any).await.interaction(),
            },
            AllCommands::UpdateMe => {

                let channel_id = interaction.channel_id;

                let result = shared.conf.with_config_mut(|conf| {
                    conf.guild_data.get_mut(&shared.guild).unwrap().notifications = channel_id;
                });

                match result {
                    Ok(_) => result_menu(&null_menu, true, "Success!").await.interaction(),
                    Err(error) => {
                        dispatch_debug(error);
                        result_menu(&null_menu, true, "Internal error. This is a bug!").await.interaction()
                    }
                }
            }
            AllCommands::Test => {
                let id = interaction.id;
                CreateInteractionResponseMessage::new()
                    .add_embed(
                        CreateEmbed::new()
                            .author(
                                CreateEmbedAuthor::new("ur mom")
                                    .icon_url("https://cdn.discordapp.com/avatars/1255856153498357800/cdcf9ad7c5c49fbf64f8bf2b7cfe07ee.png")
                            )
                            .color((3, 227, 252))
                            .description("Fancy description")
                            .image("https://i.imgflip.com/5qaljv.png")
                            .thumbnail("https://ih1.redbubble.net/image.3447065712.8364/flat,750x,075,f-pad,750x1000,f8f8f8.u5.jpg")
                            .title("YIPPIEEE")
                            .url("https://www.discord.com/")
                            .footer(
                                CreateEmbedFooter::new("footer (feet 🤤)")
                                    .icon_url("https://preview.redd.it/7d5lbusp941c1.jpeg?width=640&crop=smart&auto=webp&s=66bdf875565e74be8bc7228c1cadc074d6582e12")
                            )
                            .field("> field_name", "> value", true)
                            .field("> field_name2", "> value2 idk", true)
                    )
                    .components([
                        CreateActionRow::Buttons([
                            CreateButton::new(format!("btn1-{id}"))
                            .style(ButtonStyle::Danger)
                            .label("Evil sex")
                            .emoji('🤤'),
                            CreateButton::new(format!("btn2-{id}"))
                            .style(ButtonStyle::Secondary)
                            .label("Normal (boring) sex")
                            .emoji('😒')
                        ].to_vec())
                    ].to_vec())
                /*CreateInteractionResponse::Modal(
                    CreateModal::new(format!("test_modal-{id}"), "Test modal")
                        .components([
                            CreateActionRow::InputText(
                                CreateInputText::new(InputTextStyle::Short, "label", format!("test_modal-label-{id}"))
                                    .required(false)
                            )
                        ].to_vec())
                )*/
            }
            AllCommands::DebugEndPoll { server, mod_id } => {
                let (channel, msg) = shared.conf.with_config(|conf| {
                    let ref poll = conf.guild_data[&shared.guild].polls[&PollKey::mod_op(server, mod_id)];
                    (poll.channel, poll.poll)
                });
                
                let message = channel.message(shared, msg).await.unwrap();
                let _ = message.end_poll(shared.http()).await;
                
                info_menu("Ok").interaction()
            }
        }
    }
}

static MENUS: Lazy<std::sync::Mutex<HashMap<u128, MenuHistory>>> = Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

fn allocate_menu(menu: MenuHistory) -> u128 {
    let mut lock = MENUS.lock().unwrap();
    let mut rng = thread_rng();
    let id = loop {
        let random = rng.gen::<u128>();
        if !lock.contains_key(&random) {
            lock.insert(random, menu);
            break random;
        }
    };
    id
}

fn get_menu(id: u128) -> Option<MenuHistory> {
    let lock = MENUS.lock().unwrap();
    lock.get(&id).cloned()
}

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Display)]
enum MenuUrlKind {
    #[serde(rename = "p")]
    Page,
    #[serde(rename = "a")]
    Action
}

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct MenuUrl {
    #[serde(rename = "k")]
    kind: MenuUrlKind,
    #[serde(rename = "u")]
    url: String,
    #[serde(rename = "a")]
    arguments: Vec<String>,
}

impl MenuUrl {
    pub fn page(name: &str, args: &[&str]) -> Self {
        Self {
            kind: MenuUrlKind::Page,
            url: name.to_owned(),
            arguments: args.into_iter().map(ToString::to_string).collect(),
        }
    }

    pub fn action(name: &str, args: &[&str]) -> Self {
        Self {
            kind: MenuUrlKind::Action,
            url: name.to_owned(),
            arguments: args.into_iter().map(ToString::to_string).collect(),
        }
    }
}

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[ende(variant: bit8; size: leb128)]
struct MenuHistory {
    #[serde(rename = "h")]
    history: Vec<MenuUrl>,
    #[serde(rename = "c")]
    current: MenuUrl,
}


impl MenuHistory {
    pub fn new(main: &str) -> Self {
        Self {
            history: Vec::new(),
            current: MenuUrl::page(main, &[]),
        }
    }

    pub fn enter_page(&self, url: MenuUrl) -> Self {
        Self {
            history: {
                let mut h = self.history.clone();
                h.push(self.current.clone());
                h
            },
            current: url
        }
    }

    pub fn exit_page(&self) -> Option<Self> {
        let mut this = self.clone();
        loop {
            let current = this.history.pop()?;
            this.current = current;
            if let MenuUrlKind::Page = this.current.kind {
                break;
            }
        }
        Some(this)
    }

    pub fn to_id(&self) -> String {
        {
            let lock = MENUS.lock().unwrap();
            if let Some(id) = lock.iter().find(|(_, menu)| *menu == self).map(|(id, _)| id) {
                return id.to_string();
            }
        }
        let id = allocate_menu(self.clone());
        id.to_string()
    }

    pub fn from_id(string: impl AsRef<str>) -> Self {
        let string = string.as_ref();
        let this: Result<Self> = try {
            let id = u128::from_str(&string)?;
            let menu = get_menu(id).ok_or(anyhow!("No menu found"))?;
            // deallocate_menu(id);
            menu
        };
        this.unwrap_or(MenuHistory::new(&format!("broken-{string}")))
    }
}

// impl From<&str> for MenuHistory {
//     fn from(value: &str) -> Self {
//         let this: Result<Self> = try {
//             // let engine = general_purpose::URL_SAFE;
//             // let string = engine.decode(value)?;
//             // let mut decoder = Encoder::new(Slice::new(&string), ende::Context::new());
//             // MenuHistory::decode(&mut decoder)?
//             // let id = u128::from_str(value)?;
//             // get_menu(id)
//             Err(anyhow!())
//         };
//         this.unwrap_or(MenuHistory::new("broken"))
//     }
// }

// impl core::fmt::Display for MenuHistory {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         // let mut encoder = Encoder::new(VecStream::new(Vec::new(), 0), ende::Context::new());
//         // self.encode(&mut encoder).unwrap();
//         // let data = encoder.finish().0.into_inner();
//         // let engine = general_purpose::URL_SAFE;
//         // let mut encoded = String::new();
//         // engine.encode_string(data, &mut encoded);
//         // if encoded.len() > 100 {
//         //     let broken = MenuHistory::new("broken");
//         //     println!("Tried to encode {self:?} (too long!). Encoding {broken:?} instead");
//         //     return broken.fmt(f);
//         // }
//         let id = allocate_menu(self.clone());
//         let string = id.to_string();
//         write!(f, "{}", string)
//     }
// }

fn server_selection(history: &MenuHistory, statuses: &[ServerStatus], url: &str, url_kind: MenuUrlKind, args: &[&str]) -> (Vec<(String, String, bool)>, Vec<CreateButton>) {
    let mut fields: Vec<(String, String, bool)> = Vec::with_capacity(statuses.len());
    let mut buttons: Vec<CreateButton> = Vec::with_capacity(statuses.len());

    for status in statuses {
        fields.push((
            format!("> `{}`", escape_discord(&status.name)),
            pretty_status(status.status),
            true
        ));

        let mut new_args: Vec<&str> = Vec::with_capacity(args.len() + 1);
        new_args.push(&status.name);
        new_args.extend_from_slice(args);

        let page = match url_kind {
            MenuUrlKind::Page => MenuUrl::page(url, &new_args),
            MenuUrlKind::Action => MenuUrl::action(url, &new_args),
        };
        let page = history.enter_page(page);

        buttons.push(
            CreateButton::new(page.to_id())
                .label(status.name.clone())
                .style(ButtonStyle::Success)
        )
    }

    (fields, buttons)
}

async fn dashboard_menu(shared: &SharedMin, history: &MenuHistory) -> ProcessedMenu {
    match send_command(shared, NetCommand::ListServers) {
        Ok(Response::List(statuses)) => {
            let (fields, buttons) = server_selection(history, &statuses, "menu", MenuUrlKind::Page, &[]);

            Menu::new((3, 227, 252), "Server dashboard".to_owned(), Some("Pick which server you want to interact with".to_owned()))
                .fields(fields)
                .buttons(buttons)
                .build(history, true, true)
        }
        Ok(any) => send_unknown(history, &any).await,
        Err(any) => send_err(history, &any).await,
    }
}

async fn server_menu(shared: &SharedMin, history: &MenuHistory, server: &str) -> ProcessedMenu {
    match send_command(shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::Status)) {
        Ok(Response::Status(status)) => {
            let mut buttons: Vec<CreateButton> = Vec::with_capacity(10);
            match status.status {
                Status::Idle | Status::Stopping => {
                    let action = MenuUrl::action("start", &[server]);
                    let action = history.enter_page(action);

                    buttons.push(CreateButton::new(action.to_id()).label("Start").emoji('▶').style(ButtonStyle::Success));
                }
                Status::Running | Status::Starting => {
                    let action = MenuUrl::action("stop", &[server]);
                    let action = history.enter_page(action);

                    buttons.push(CreateButton::new(action.to_id()).label("Stop").emoji('🛑').style(ButtonStyle::Danger));
                }
                _ => {}
            }
            let action = MenuUrl::action("reboot", &[server]);
            let action = history.enter_page(action);

            buttons.push(CreateButton::new(action.to_id()).label("Reboot").emoji('🔄').style(ButtonStyle::Success));

            if let Status::Running = status.status {
                let action = MenuUrl::action("command", &[server]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_id()).label("Command").emoji('🔑').style(ButtonStyle::Secondary));
            }
            if let Status::Idle = status.status {
                let page = MenuUrl::page("mods", &[server, &0u64.to_string()]);
                let page = history.enter_page(page);

                buttons.push(CreateButton::new(page.to_id()).label("Mods").emoji('🎲').style(ButtonStyle::Primary));

                let action = MenuUrl::action("backup", &[server]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_id()).label("Backup").emoji('💾').style(ButtonStyle::Danger));

                let action = MenuUrl::action("restore", &[server]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_id()).label("Restore").emoji('↩').style(ButtonStyle::Danger));
            }
            
            if status.status != Status::Modding {
                let action = MenuUrl::action("zip", &[server]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_id()).label("Package mods").emoji('📦').style(ButtonStyle::Danger));
            }

            Menu::new((245, 167, 66), server.to_owned(), None)
                .fields([
                    ("Status".to_string(), format!("{}", pretty_status(status.status)), false),
                    ("Path".to_string(), format!("{}", escape_discord(status.path)), false),
                ].to_vec())
                .buttons(buttons)
                .build(history, true, true)
        }
        Ok(any) => send_unknown(history, &any).await,
        Err(any) => send_err(history, &any).await,
    }
}

async fn mod_menu(shared: &SharedMin, history: &MenuHistory, server: &str, page: u64) -> ProcessedMenu {
    match send_command(shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::ListMods(10, page))) {
        Ok(Response::Mods(mods, finished)) => {
            let mut buttons: Vec<CreateButton> = Vec::new();
            let mut fields: Vec<(String, String, bool)> = Vec::new();

            if page != 0 {
                let mut menu_page = history.clone();
                menu_page.current = MenuUrl::page("mods", &[server, &(page - 1).to_string()]);

                buttons.push(CreateButton::new(menu_page.to_id()).label("Previous").style(ButtonStyle::Primary));
            }
            if !finished {
                let mut menu_page = history.clone();
                menu_page.current = MenuUrl::page("mods", &[server, &(page + 1).to_string()]);

                buttons.push(CreateButton::new(menu_page.to_id()).label("Next").style(ButtonStyle::Primary));
            }

            for modd in mods {
                let action = MenuUrl::page("mod", &[server, &modd.mod_id]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_id()).label(&modd.name.clone().unwrap_or(modd.mod_id.clone())).style(ButtonStyle::Success));

                fields.push((modd.mod_id.clone(), modd.name.clone().unwrap_or(modd.mod_id.clone()), true));
            }

            Menu::new((192, 63, 196), server.to_owned(), None)
                .fields(fields)
                .buttons(buttons)
                .build(history, true, true)
        }
        Ok(any) => send_unknown(history, &any).await,
        Err(any) => send_err(history, &any).await,
    }
}

async fn single_mod_menu(shared: &SharedMin, history: &MenuHistory, server: &str, mod_id: &str) -> (ProcessedMenu, Option<Vec<u8>>) {
    match send_command(shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::QueryMod(mod_id.to_owned()))) {
        Ok(Response::Mod(modd)) => {
            let mut buttons = Vec::new();
            let action = MenuUrl::action("uninstall", &[server, mod_id]);
            let action = history.enter_page(action);

            if !reserved_mod_id(mod_id) {
                buttons.push(CreateButton::new(action.to_id()).label("Uninstall").emoji('🗑').style(ButtonStyle::Danger));
            }

            let menu = Menu::from_embed(mod_embed(&modd)).buttons(buttons);

            (menu.build(history, true, false), modd.logo)
        }
        Ok(any) => (send_unknown(history, &any).await, None),
        Err(any) => (send_err(history, &any).await, None),
    }
}

async fn result_menu(history: &MenuHistory, success: bool, message: &str) -> ProcessedMenu {
    let title = if success {
        ":white_check_mark: Success"
    } else {
        ":x: Error"
    };
    let color = if success {
        (144, 245, 66)
    } else {
        (235, 64, 66)
    };
    Menu::new(color, title.to_owned(), Some(message.to_owned()))
        .build(history, true, false)
}

fn outdated() -> ProcessedMenu {
    Menu::new((235, 64, 66), "Menu is outdated (the bot was restarted maybe?)".to_owned(), None)
        .build(&MenuHistory::new("null"), false, false)
}

fn no_such_mod(mod_id: &str, server: &str) -> ProcessedMenu {
    info_menu(&format!(r#"Can't remove "{}" because it's not installed on "{}"!"#, escape_discord(mod_id), escape_discord(server)))
}

fn already_installed(mod_id: &str, server: &str) -> ProcessedMenu {
    info_menu(&format!(r#"Mod "{}" is already installed for "{}"!"#, escape_discord(mod_id), escape_discord(server)))
}

fn info_menu(message: &str) -> ProcessedMenu {
    let color = (36, 146, 224);
    Menu::new(color, message.to_owned(), None)
        .build(&MenuHistory::new("info_menu"), false, false)
}

async fn wtf_bad_bot(history: &MenuHistory) -> ProcessedMenu {
    Menu::new((0, 0, 0), "Wtf bad bot".to_string(), Some(format!("{history:?}")))
        .build(history, false, false)
}

async fn install_mod_menu(shared: &SharedMin, history: &MenuHistory, channel_id: &str, message_id: &str) -> ProcessedMenu {
    match send_command(shared, NetCommand::ListServers) {
        Ok(Response::List(statuses)) => {
            let (fields, buttons) = server_selection(history, &statuses, "install", MenuUrlKind::Action, &[channel_id, message_id]);

            Menu::new((3, 227, 252), "Install mod for...".to_owned(), None)
                .fields(fields)
                .buttons(buttons)
                .build(history, true, false)
        }
        Ok(any) => send_unknown(history, &any).await,
        Err(any) => send_err(history, &any).await,
    }
}

struct Handler {
    shared: Arc<SharedMin>
}

impl Handler {
    pub fn new(shared: Arc<SharedMin>) -> Self {
        Self { shared }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn message_delete(&self, ctx: Context, channel_id: ChannelId, deleted_message_id: MessageId, guild_id: Option<GuildId>) {
        let Some(guild_id) = guild_id else { return };
        if guild_id != self.shared.guild {
            return;
        }

        poll_deleted(&self.shared.to_full(ctx.http), channel_id, deleted_message_id).await;
    }

    async fn poll_vote_add(&self, ctx: Context, event: MessagePollVoteAddEvent) {
        let Some(guild_id) = event.guild_id else { return };
        if guild_id != self.shared.guild {
            return;
        }

        check_poll(&self.shared.to_full(ctx.http), event.channel_id, event.message_id).await;
    }

    async fn message(&self, ctx: Context, new_message: Message) {
        let Some(guild_id) = new_message.guild_id else { return };
        if guild_id != self.shared.guild {
            return;
        }

        // Detect when a poll ends
        if let Some(embed) = new_message.embeds.get(0) &&
            let Some(kind) = &embed.kind && kind == "poll_result" &&
            let Some(reference) = &new_message.message_reference &&
            let Some(reference_msg) = reference.message_id &&
            let Ok(the_poll) = reference.channel_id.message(&ctx, reference_msg).await
        {
            check_poll(&self.shared.to_full(ctx.http.clone()), the_poll.channel_id, the_poll.id).await;
        }

        if new_message.author.bot {
            return;
        }
        if let Ok(true) = new_message.mentions_me(&ctx).await {} else { return }

        if let Some(part) = new_message.content.split(" ").find(|x| Url::parse(x).is_ok()) &&
            let Ok(url) = Url::parse(part) && url.scheme() == "https" &&
            let Some(host) = url.host() &&
            let Host::Domain("www.curseforge.com") = host &&
            let Some(mut segments) = url.path_segments() &&
            let Some("minecraft") = segments.next() &&
            let Some("mc-mods") = segments.next() &&
            let Some(mod_name) = segments.next()
        {
            // let request = format!("https://www.curseforge.com/minecraft/mc-mods/estrogen/files/5099451");
            // let _ = new_message.reply(
            //     &ctx,
            //     format!("You posted a link to a mc mod: {mod_name}\nI will create a request to this url: {request}")
            // ).await;
            //
            // let result: Result<()> = try {
            //     let response = reqwest::get(request).await.context("Error sending request")?;
            //     let bytes = response.bytes().await.context("Error reading response body")?;
            //     println!("{}", core::str::from_utf8(bytes.borrow()).context("Couldn't convert to string")?);
            // };
            // if let Err(err) = result {
            //     let _ = new_message.reply(&ctx, format!("There was an error: {err:?}")).await;
            // }
            let _ = new_message.reply(&ctx, format!(r#"I detected you're trying to install "{mod_name}", but i can't access curseforge (yet ;))). For now, please upload the mod file directly"#)).await;
        } else if new_message.attachments.len() == 1 {
            let channel_id = base64_encode(new_message.channel_id.get().to_le_bytes());
            let message_id = base64_encode(new_message.id.get().to_le_bytes());

            let history = MenuHistory::new("install");
            let msg = install_mod_menu(
                &self.shared,
                &history,
                &channel_id,
                &message_id
            ).await;
            let _ = new_message.channel_id.send_message(&ctx, msg.message().reference_message(&new_message)).await;
        } else if new_message.attachments.len() > 1 {
            let _ = new_message.reply(&ctx, "Only 1 attachment at a time pls!").await;
        } else {
            const MESSAGES: &[&str] = &[
                "HELLO",
                "Hallo :3",
                "Hi",
                "Ur annoying",
                "HIIIIIIIIII",
                "https://tenor.com/view/blm-gif-25815938",
                r#"Minecraft is a 2011 sandbox game developed by Mojang Studios and originally released in 2009. The game was created by Markus "Notch" Persson in the Java programming language. Following several early private testing versions, it was first made public in May 2009 before being fully released on November 18, 2011, with Notch stepping down and Jens "Jeb" Bergensten taking over development. Minecraft has become the best-selling video game in history, with over 300 million copies sold and nearly 140 million monthly active players as of 2023. Over the years following its release, it has been ported to several platforms, including smartphones and various consoles.

In Minecraft, players explore a blocky, pixelated, procedurally generated, three-dimensional world with virtually infinite terrain. Players can discover and extract raw materials, craft tools and items, and build structures, earthworks, and machines. Depending on their chosen game mode, players can fight hostile mobs, as well as cooperate with or compete against other players in the same world. Game modes include a survival mode (in which players must acquire resources to build in the world and maintain health), creative mode (in which players have unlimited resources and the ability to fly), spectator mode (in which players can fly, go through blocks, and enter the bodies of other players and entities), adventure mode (in which players have to survive without being able to build and place blocks), and hardcore mode (in which the difficulty is set to Hard and dying causes the player to lose their ability to play on that world). The game's large community also offers a wide variety of user-generated content, such as modifications, servers, skins, texture packs, and custom maps, which add new game mechanics and possibilities. "#,
                "hewwo :3c",
            ];

            let (should_answer, random) = {
                let mut rng = StdRng::from_entropy();
                let should_answer = rng.gen_bool(0.3);
                let random = rng.gen_range(0..MESSAGES.len());
                (should_answer, random)
            };

            if should_answer {
                let _ = new_message.reply(&ctx, MESSAGES[random]).await;
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        for guild in ready.guilds.iter() {
            let _ = guild
                .id
                .set_commands(&ctx, AllCommands::create_commands())
                .await;
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let guild_id = match &interaction {
            Interaction::Autocomplete(x) => x.guild_id,
            Interaction::Command(x) => x.guild_id,
            Interaction::Component(x) => x.guild_id,
            Interaction::Modal(x) => x.guild_id,
            _ => return,
        };
        let Some(guild_id) = guild_id else { return };
        if guild_id != self.shared.guild {
            return;
        }

        const CONFIRM: &str = "yes, i confirm";
        const CONFIRM_BACKUP: &str = "the last backup will be lost";
        const CONFIRM_RESTORE: &str = "all changes will be lost";

        match interaction {
            Interaction::Command(command) => {
                let command_data = AllCommands::from_command_data(&command.data).unwrap();
                let resp = command
                    .create_response(&ctx.http, CreateInteractionResponse::Message(command_data.run(&self.shared.to_full(ctx.http.clone()), &command).await))
                    .await;
                if let Err(err) = resp {
                    dispatch_debug(err);
                }
            }
            Interaction::Component(comp) => {
                let id = &comp.data.custom_id;
                let h = MenuHistory::from_id(id);

                let response = match h.current.kind {
                    MenuUrlKind::Page => {
                        match &h.current.url as &str {
                            "dashboard" => {
                                CreateInteractionResponse::UpdateMessage(dashboard_menu(&self.shared, &h).await.interaction())
                            }
                            "menu" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(server_menu(&self.shared, &h, server).await.interaction())
                            }
                            "install" => {
                                let channel_id = &h.current.arguments[0];
                                let message_id = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(install_mod_menu(&self.shared, &h, channel_id, message_id).await.interaction())
                            }
                            "mods" => {
                                let server = &h.current.arguments[0];
                                let page = &h.current.arguments[1];
                                let page = u64::from_str(page).unwrap();
                                CreateInteractionResponse::UpdateMessage(mod_menu(&self.shared, &h, server, page).await.interaction())
                            }
                            "mod" => {
                                let server = &h.current.arguments[0];
                                let mod_id = &h.current.arguments[1];
                                let (menu, att) = single_mod_menu(&self.shared, &h, server, mod_id).await;
                                let mut msg = menu.interaction();
                                if let Some(att) = att {
                                    msg = msg.files([CreateAttachment::bytes(att, "logo.png")]);
                                }
                                CreateInteractionResponse::UpdateMessage(msg)
                            }
                            any if any.starts_with("broken-") => {
                                CreateInteractionResponse::UpdateMessage(outdated().interaction())
                            }
                            _ => {
                                CreateInteractionResponse::UpdateMessage(wtf_bad_bot(&h).await.interaction())
                            }
                        }
                    }
                    MenuUrlKind::Action => {
                        match &h.current.url as &str {
                            "start" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(
                                    match send_command(&self.shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::Start)) {
                                        Ok(Response::Ok) => result_menu(&h, true, "Server started!").await.interaction(),
                                        Ok(any) => send_unknown(&h, &any).await.interaction(),
                                        Err(any) => send_err(&h, &any).await.interaction(),
                                })
                            }
                            "stop" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(
                                    match send_command(&self.shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::Stop)) {
                                        Ok(Response::Ok) => result_menu(&h, true, "Server stopped.").await.interaction(),
                                        Ok(any) => send_unknown(&h, &any).await.interaction(),
                                        Err(any) => send_err(&h, &any).await.interaction(),
                                })
                            }
                            "reboot" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(
                                    match send_command(&self.shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::Reboot)) {
                                        Ok(Response::Ok) => result_menu(&h, true, "Server is rebooting!").await.interaction(),
                                        Ok(any) => send_unknown(&h, &any).await.interaction(),
                                        Err(any) => send_err(&h, &any).await.interaction(),
                                })
                            }
                            "command" => {
                                let server = &h.current.arguments[0];

                                CreateInteractionResponse::Modal(
                                    CreateModal::new(h.to_id(), format!("Input command for `{}`", escape_discord(server)))
                                        .components([
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, "command", "command_field"))
                                        ].to_vec())
                                )
                            }
                            "backup" => {
                                let server = &h.current.arguments[0];

                                CreateInteractionResponse::Modal(
                                    CreateModal::new(h.to_id(), format!("Backup `{}`?", escape_discord(server)))
                                        .components([
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, format!(r#"type "{CONFIRM}""#), "confirm_field1")),
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, format!(r#"type "{CONFIRM_BACKUP}""#), "confirm_field2")),
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, "type the name of the server", "confirm_field3"))
                                        ].to_vec())
                                )
                            }
                            "restore" => {
                                let server = &h.current.arguments[0];

                                CreateInteractionResponse::Modal(
                                    CreateModal::new(h.to_id(), format!("Restore `{}`?", escape_discord(server)))
                                        .components([
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, format!(r#"type "{CONFIRM}""#), "confirm_field1")),
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, format!(r#"type "{CONFIRM_RESTORE}""#), "confirm_field2")),
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, "type the name of the server", "confirm_field3"))
                                        ].to_vec())
                                )
                            }
                            "install" => {
                                let server = &h.current.arguments[0];
                                let channel_id = &h.current.arguments[1];
                                let msg_id = &h.current.arguments[2];

                                let channel_id = base64_decode(channel_id);
                                let msg_id = base64_decode(msg_id);

                                let channel_id = ende::decode(channel_id.as_slice()).unwrap();
                                let msg_id = ende::decode(msg_id.as_slice()).unwrap();

                                let channel_id = ChannelId::new(channel_id);
                                let msg_id = MessageId::new(msg_id);

                                let get_msg = ctx.http.get_message(channel_id, msg_id);
                                let del_msg = comp.message.delete(&ctx);

                                let (get_msg, _) = join!(get_msg, del_msg);

                                match get_msg {
                                    Ok(msg) if msg.attachments.len() == 1 => {
                                        let data = IdleMod {
                                            server: server.clone(),
                                            att_name: msg.attachments[0].filename.clone(),
                                            url: msg.attachments[0].url.clone().parse().unwrap(),
                                        };

                                        let shared = Arc::new(self.shared.to_full(ctx.http.clone()));
                                        
                                        tokio::spawn(async move { process_mods::mod_thread(shared, data).await });
                                    }
                                    _ => {
                                        let msg1 = channel_id.send_message(&ctx, CreateMessage::new().content("You changed the message while i was processing it"));
                                        let msg2 = channel_id.send_message(&ctx, CreateMessage::new().content("https://tenor.com/view/blm-gif-25815938"));
                                        let _ = join!(msg1, msg2);
                                    }
                                }

                                CreateInteractionResponse::Acknowledge
                            }
                            "uninstall" => {
                                let server = &h.current.arguments[0];
                                let mod_id = &h.current.arguments[1];

                                match send_command(&self.shared, NetCommand::ServerCommand(server.clone(), ServerCommand::QueryMod(mod_id.clone()))) {
                                    Ok(Response::Mod(info)) => {
                                        let key = PollKey::Mod {
                                            server: server.clone(),
                                            mod_id: info.mod_id.clone(),
                                        };
                                        
                                        let kind = PollKind::Remove {
                                            server: server.clone(),
                                            info,
                                        };
                                        
                                        create_poll(&self.shared.to_full(ctx.http.clone()), key, kind).await;

                                        CreateInteractionResponse::Acknowledge
                                    }
                                    Ok(any) => CreateInteractionResponse::UpdateMessage(send_unknown(&h, &any).await.interaction()),
                                    Err(any) => CreateInteractionResponse::UpdateMessage(send_err(&h, &any).await.interaction()),
                                }
                            }
                            "zip" => {
                                let server = &h.current.arguments[0];

                                match send_command(&self.shared, NetCommand::ServerCommand(server.clone(), ServerCommand::GenerateModsZip)) {
                                    Ok(Response::Ok) => {
                                        CreateInteractionResponse::UpdateMessage(result_menu(&h, true, "Mods zip file is generating!").await.interaction())
                                    }
                                    Ok(any) => CreateInteractionResponse::UpdateMessage(send_unknown(&h, &any).await.interaction()),
                                    Err(any) => CreateInteractionResponse::UpdateMessage(send_err(&h, &any).await.interaction()),
                                }
                            }
                            any if any.starts_with("broken-") => {
                                CreateInteractionResponse::UpdateMessage(outdated().interaction())
                            }
                            _ => {
                                CreateInteractionResponse::UpdateMessage(wtf_bad_bot(&h).await.interaction())
                            }
                        }
                    }
                };

                let resp = comp.create_response(&ctx.http, response).await;
                if let Err(err) = resp {
                    dispatch_debug(err);
                }
            }
            Interaction::Modal(modal) => {
                let id = &modal.data.custom_id;
                let h = MenuHistory::from_id(id);

                let response = match h.current.kind {
                    MenuUrlKind::Page => {
                        match &h.current.url as &str {
                            _ => {
                                CreateInteractionResponse::Message(wtf_bad_bot(&h).await.interaction())
                            }
                        }
                    }
                    MenuUrlKind::Action => {
                        match &h.current.url as &str {
                            "command" => {
                                let server = &h.current.arguments[0];
                                let ActionRowComponent::InputText(text) = &modal.data.components[0].components[0] else { panic!() };
                                let command = text.value.as_ref().unwrap();

                                CreateInteractionResponse::UpdateMessage(
                                    match send_command(&self.shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::Console(command.clone()))) {
                                        Ok(Response::CommandOutput(output)) => result_menu(&h, true, &output).await.interaction(),
                                        Ok(Response::UnknownServer) => unknown_server(&h, &server).await.interaction(),
                                        Ok(any) => send_unknown(&h, &any).await.interaction(),
                                        Err(any) => send_err(&h, &any).await.interaction(),
                                })
                            }
                            "backup" => {
                                let server = &h.current.arguments[0];
                                let ActionRowComponent::InputText(confirm1) = &modal.data.components[0].components[0] else { panic!() };
                                let ActionRowComponent::InputText(confirm2) = &modal.data.components[1].components[0] else { panic!() };
                                let ActionRowComponent::InputText(confirm3) = &modal.data.components[2].components[0] else { panic!() };
                                let confirm1 = confirm1.value.as_ref().unwrap();
                                let confirm2 = confirm2.value.as_ref().unwrap();
                                let confirm3 = confirm3.value.as_ref().unwrap();

                                CreateInteractionResponse::UpdateMessage(
                                    if confirm1.eq_ignore_ascii_case(CONFIRM) &&
                                        confirm2.eq_ignore_ascii_case(CONFIRM_BACKUP) &&
                                        confirm3 == server
                                    {
                                        match send_command(&self.shared, NetCommand::ServerCommand(server.clone(), ServerCommand::Backup)) {
                                            Ok(Response::Ok) => result_menu(&h, true, "Backing up!").await.interaction(),
                                            Ok(Response::UnknownServer) => unknown_server(&h, &server).await.interaction(),
                                            Ok(any) => send_unknown(&h, &any).await.interaction(),
                                            Err(any) => send_err(&h, &any).await.interaction(),
                                        }
                                    } else {
                                        result_menu(&h, false, "Please properly confirm this action!").await.interaction()
                                    }
                                )
                            }
                            "restore" => {
                                let server = &h.current.arguments[0];
                                let ActionRowComponent::InputText(confirm1) = &modal.data.components[0].components[0] else { panic!() };
                                let ActionRowComponent::InputText(confirm2) = &modal.data.components[1].components[0] else { panic!() };
                                let ActionRowComponent::InputText(confirm3) = &modal.data.components[2].components[0] else { panic!() };
                                let confirm1 = confirm1.value.as_ref().unwrap();
                                let confirm2 = confirm2.value.as_ref().unwrap();
                                let confirm3 = confirm3.value.as_ref().unwrap();

                                CreateInteractionResponse::UpdateMessage(
                                    if confirm1.eq_ignore_ascii_case(CONFIRM) &&
                                        confirm2.eq_ignore_ascii_case(CONFIRM_RESTORE) &&
                                        confirm3 == server
                                    {
                                        match send_command(&self.shared, NetCommand::ServerCommand(server.clone(), ServerCommand::Restore)) {
                                            Ok(Response::Ok) => result_menu(&h, true, "Restoring backup!").await.interaction(),
                                            Ok(Response::UnknownServer) => unknown_server(&h, &server).await.interaction(),
                                            Ok(Response::NoBackup) => result_menu(&h, false, "No backup exists!").await.interaction(),
                                            Ok(any) => send_unknown(&h, &any).await.interaction(),
                                            Err(any) => send_err(&h, &any).await.interaction(),
                                        }
                                    } else {
                                        result_menu(&h, false, "Please properly confirm this action!").await.interaction()
                                    }
                                )
                            }
                            any if any.starts_with("broken-") => {
                                CreateInteractionResponse::UpdateMessage(outdated().interaction())
                            }
                            _ => {
                                CreateInteractionResponse::UpdateMessage(wtf_bad_bot(&h).await.interaction())
                            }
                        }
                    }
                };

                let resp = modal.create_response(&ctx.http, response).await;
                if let Err(err) = resp {
                    dispatch_debug(err);
                }
            }
            _ => {}
        }
    }
}

pub struct NotifServer {
    channel_messages: HashMap<ChannelId, MessageId>
}

impl NotifServer {
    fn clear(&mut self) {
        self.channel_messages.clear();
    }

    async fn send_msg(&mut self, cache: impl CacheHttp, channel: impl Into<ChannelId>, msg: impl Into<String>) {
        let channel = channel.into();
        if let Some(message) = self.channel_messages.get(&channel) {
            let msg = info_menu(&msg.into()).edit_message();
            let _ = channel.edit_message(cache, message, msg).await;
        } else {
            let msg = info_menu(&msg.into()).message();
            let Ok(message) = channel.send_message(cache, msg).await else { return };
            self.channel_messages.insert(channel, message.id);
        }
    }

    async fn delete_msg(&mut self, cache: impl CacheHttp, channel: impl Into<ChannelId>) {
        let channel = channel.into();
        if let Some(message) = self.channel_messages.get(&channel) {
            let _ = channel.delete_message(cache.http(), message).await;
        }
        self.channel_messages.remove(&channel);
    }
}

pub struct NotifGroup {
    servers: HashMap<String, NotifServer>
}

impl NotifGroup {
    fn get(&mut self, server: &str) -> &mut NotifServer {
        if !self.servers.contains_key(server) {
            self.servers.insert(server.to_owned(), NotifServer {
                channel_messages: HashMap::new(),
            });
        }

        self.servers.get_mut(server).unwrap()
    }
}

pub struct Notifs {
    groups: HashMap<String, NotifGroup>
}

impl Notifs {
    fn new(groups: &[&str]) -> Self {
        let mut map = HashMap::new();
        for group in groups {
            map.insert(group.to_string(), NotifGroup {
                servers: HashMap::new()
            });
        }
        Self {
            groups: map
        }
    }

    fn get(&mut self, group: &str) -> &mut NotifGroup {
        self.groups.get_mut(group).unwrap()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdleMod {
    pub server: String,
    pub att_name: String,
    pub url: Url,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadedMod {
    pub server: String,
    pub att_name: String,
    pub file: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessedMod {
    pub server: String,
    pub info: ModInfo,
}
    
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FuckedUpMod {
    pub server: String,
    pub att_name: String,
    pub err: String
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModInstalling {
    Idle(IdleMod),
    Downloaded(DownloadedMod),
    Processed(ProcessedMod),
    FuckedUp(FuckedUpMod)
}

#[derive(Debug)]
enum Event {
    ServerDeleted(String),
    ServerCreated(String),
    PathChanged(String, String, String),
}

impl core::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::ServerDeleted(name) => write!(f, "Server `{}` was **deleted**", escape_discord(name)),
            Event::ServerCreated(name) => write!(f, "New server was **created**: `{}`", escape_discord(name)),
            Event::PathChanged(name, old_path, new_path) => write!(f, "Server `{}` changed its path from `{}` to `{}`", escape_discord(name), escape_discord(old_path), escape_discord(new_path)),
        }
    }
}

pub struct SharedMin {
    pub conf: Config<MCAYB>,
    pub guild: GuildId,
    pub msgs: Mutex<Notifs>,
    pub mods: Mutex<AllocVec<ModInstalling>>,
    pub last_status: Mutex<Vec<ServerStatus>>,
}

impl SharedMin {
    pub fn new(conf: Config<MCAYB>, guild: GuildId) -> Self {
        Self {
            conf: conf.clone(),
            guild,
            msgs: Mutex::new(Notifs::new(&["backup", "restore", "install_mod", "uninstall_mod", "zip"])),
            mods: Mutex::new(AllocVec::new()),
            last_status: Mutex::new(conf.with_config(|conf| conf.guild_data[&guild].last_status.clone())),
        }
    }
    
    pub fn to_full(self: &Arc<Self>, http: Arc<Http>) -> Shared {
        let this = self.clone();
        Shared {
            __composite: this,
            http,
        }
    }

    pub async fn mods_allocate(&self, modd: ModInstalling) -> usize {
        self.mods.lock().await.allocate(modd)
    }

    pub async fn mods_with<F, R>(&self, f: F) -> R
    where F: FnOnce(MutexGuard<AllocVec<ModInstalling>>) -> R
    {
        let mods = self.mods.lock().await;
        f(mods)
    }
    
    pub async fn mods_get(&self, slot: usize) -> ModInstalling {
        self.mods.lock().await[slot].clone()
    }
    
    pub async fn mods_set(&self, slot: usize, modd: ModInstalling) {
        self.mods.lock().await[slot] = modd;
    }
}

pub struct Shared {
    __composite: Arc<SharedMin>,
    pub http: Arc<Http>
}

impl Deref for Shared {
    type Target = SharedMin;
    fn deref(&self) -> &Self::Target {
        &self.__composite
    }
}

impl Shared {
    pub fn new(conf: Config<MCAYB>, http: Arc<Http>, guild: GuildId) -> Self {
        Self {
            http,
            __composite: Arc::new(SharedMin::new(conf, guild))
        }
    }
}

impl CacheHttp for Shared {
    fn http(&self) -> &Http {
        self.http.http()
    }

    fn cache(&self) -> Option<&Arc<Cache>> {
        self.http.cache()
    }
}

pub async fn init(conf: Config<MCAYB>) -> Result<()> {
    use anyhow::Context;

    // Log into discord bot
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGE_POLLS
        ;

    
    let mut client = Client::builder(TOKEN, intents);
    let mut shared_map = Vec::new();
    conf.with_config(|config| {
        for (guild, _) in config.guild_data.iter() {
            let shared = SharedMin::new(conf.clone(), *guild);
            shared_map.push(Arc::new(shared));
        }
    });
    
    for shared in shared_map.iter() {
        client = client.event_handler(Handler::new(shared.clone()));
    }

    let mut client = client.await
        .context("Failed to log into discord bot")?;
    
    let http = client.http.clone();
    
    let _handle = tokio::spawn(async move {
        if let Err(err) = client.start().await {
            dispatch_debug(&err);
        };
    });

    let data = conf.with_config(|conf| {
        conf.guild_data.clone()
    });

    let mut shared_map: Vec<Arc<Shared>> = shared_map
        .iter()
        .map(|min| min.to_full(http.clone()))
        .map(|full| Arc::new(full))
        .collect();
    
    // Check for deleted polls and polls that have already ended
    for (guild, data) in data.iter() {
        let shared = shared_map.iter().find(|shared| shared.guild == *guild).unwrap();
        for (_, poll) in data.polls.iter() {
            if !poll_deleted(&shared, poll.channel, poll.poll).await {
                check_poll(&shared, poll.channel, poll.poll).await;
            }
        }
    }

    // Check if this is a new version
    let old_version = conf.with_config_mut(|conf| {
        let version = replace(&mut conf.version, VERSION);
        version != VERSION
    }).expect("Failed to update version in config");

    if old_version {
        for (i, data) in data.values().enumerate() {
            let channel = data.notifications;
            let title = format!("Bot updated to `{VERSION}`!");
            let msg = Menu::new((28, 201, 121), title, Some(include_str!("../changelog.md").to_owned()))
                .footer(&VERSION.to_string())
                .build(&MenuHistory::new("null"), false, false)
                .message();

            let _ = channel.send_message(&shared_map[i], msg).await;
        }
    }
    
    let set = LocalSet::new();
    set.run_until(async {
        for shared in shared_map {
            let _handle = spawn_local(async move { account_thread(shared).await });
        }
        
        loop {
            yield_now().await;
        }
    }).await;
    
    // let set = LocalSet::new();
    // set.run_until(async move {
    //     conf.with_config(|config| {
    //         for (guild, _) in config.guild_data.iter() {
    //             let guild = *guild;
    //             let http = http.clone();
    //             let conf = conf.clone();
    //             let handle = tokio::task::spawn_local(async move { account_thread(guild, http, conf).await });
    //         }
    //     });
    //     
    //     loop {
    //         tokio::task::yield_now().await;
    //     }
    // }).await;
    

    Ok(())
}

async fn account_thread(shared: Arc<Shared>) {
    let mut last = SystemTime::now();
    loop {
        let now = SystemTime::now();
        if now.duration_since(last).unwrap() > Duration::from_secs(1) {
            last = now;
            guild_loop(&shared).await;
        }
    }
}

async fn guild_loop(shared: &Shared) {
    let notif_channel = shared.conf.with_config(|config| {
        config.guild_data[&shared.guild].notifications
    });

    // Detect notifications the server can't give us
    let mut msgs = shared.msgs.lock().await;
    let mut last_status = { shared.last_status.lock().await.clone() };
    match send_command(shared, NetCommand::ListServers) {
        Ok(Response::List(new_status)) => {
            let mut events = Vec::new();

            for old in last_status.iter() {
                if let Some(new) = new_status.iter().find(|x| x.name == old.name) {
                    if old.path != new.path {
                        events.push(Event::PathChanged(old.name.clone(), old.path.clone(), new.path.clone()));
                    }
                } else {
                    events.push(Event::ServerDeleted(old.name.clone()))
                }
            }

            for status in new_status.iter() {
                if !last_status.iter().any(|x| x.name == status.name) {
                    events.push(Event::ServerCreated(status.name.clone()));
                }
            }

            if new_status != last_status {
                last_status = new_status.clone();
                let _ = shared.conf.with_config_mut(|conf| {
                    conf.guild_data.get_mut(&shared.guild).unwrap().last_status = new_status;
                });
            }


            for event in events {
                let msg = info_menu(&event.to_string()).message();

                let _ = notif_channel.send_message(shared, msg).await;
            }
        }
        _ => {}
    }

    // Receive normal notifications
    match send_command(shared, NetCommand::Notifications) {
        Ok(Response::Notifications(notifs)) => {
            for notif in notifs {
                match &notif {
                    Notification::BackupProgress(server, _, _) => {
                        msgs.get("restore").get(server).send_msg(shared, notif_channel, notif.to_string()).await;
                    }
                    Notification::RestoreProgress(server, _, _) => {
                        msgs.get("backup").get(server).send_msg(shared, notif_channel, notif.to_string()).await;
                    }
                    Notification::ZipProgress(server, _) => {
                        msgs.get("zip").get(server).send_msg(shared, notif_channel, notif.to_string()).await;
                    }
                    Notification::ZipFailed(server, _) => {
                        msgs.get("zip").get(server).send_msg(shared, notif_channel, notif.to_string()).await;
                        msgs.get("zip").get(server).clear();
                    }
                    Notification::ZipFile(server, _) => {
                        msgs.get("zip").get(server).send_msg(shared, notif_channel, notif.to_string()).await;
                        msgs.get("zip").get(server).clear();
                    }
                    notif => {
                        let msg = info_menu(&notif.to_string()).message();

                        let _ = notif_channel.send_message(shared, msg).await;
                    }
                }

                // Reset messages statuses
                if let Notification::StatusChanged(server, _, new) = &notif {
                    if *new != Status::BackingUp {
                        msgs.get("backup").get(server).clear();
                    }
                    if *new != Status::Restoring {
                        msgs.get("restore").get(server).clear();
                    }
                }
            }
        }
        _ => {}
    }

    let mut mods = shared.mods.lock().await;
    if mods.is_empty() {
        msgs.get("install_mod").servers.clear();
    }

    let mut to_remove = Vec::new();
    for (idx, modd) in mods.enumerate() {
        match modd {
            ModInstalling::Idle(IdleMod { server, att_name, .. }) => {
                let data = format!(r#""{}" sent for downloading  "#, escape_discord(att_name));
                msgs.get("install_mod").get(server).send_msg(shared, notif_channel, data.clone()).await;
            }
            ModInstalling::Downloaded(DownloadedMod { server, att_name, .. }) => {
                let data = format!(r#"Downloaded "{}""#, escape_discord(att_name));
                msgs.get("install_mod").get(server).send_msg(shared, notif_channel, data.clone()).await;
            }
            ModInstalling::Processed(processed) => {
                to_remove.push(idx);
                msgs.get("install_mod").get(&processed.server).delete_msg(shared, notif_channel).await;

                let key = PollKey::Mod {
                    server: processed.server.clone(),
                    mod_id: processed.info.mod_id.clone()
                };
                
                let kind = PollKind::Install {
                    server: processed.server.clone(),
                    info: processed.info.clone(),
                };
                
                create_poll(shared, key, kind).await;
            }
            ModInstalling::FuckedUp(FuckedUpMod { server, att_name, err }) => {
                to_remove.push(idx);
                let data = format!(r#"Error processing mod "{}": {err:?}"#, escape_discord(att_name));
                msgs.get("install_mod").get(server).send_msg(shared, notif_channel, data.clone()).await;
            }
        }
    }

    for idx in to_remove {
        mods.deallocate(idx);
    }
}

async fn create_poll(shared: &Shared, key: PollKey, kind: PollKind) {
    use anyhow::Context;

    // Failure conditions
    let existing_poll = shared.conf.with_config(|config| {
        config.guild_data[&shared.guild].polls.get(&key).cloned()
    });
    let channel = shared.conf.with_config(|conf| conf.guild_data[&shared.guild].notifications);
    
    // Poll for this mod and server already exists

    // Edge case where the poll has been deleted:
    // The second check will fail, and the poll will be overwritten in the config
    if let Some(poll) = existing_poll && let Ok(the_poll) = shared.http().get_message(poll.channel, poll.poll).await {
        let description = match &poll.kind {
            PollKind::Install { info, server, .. } |
            PollKind::Remove { info, server, .. } => {
                format!(r#"A poll involving "{}" in "{}" has already been made!"#, escape_discord(info.name()), escape_discord(server))
            }
            PollKind::Restore { server, .. } => {
                format!(r#"A poll involving server "{}" has already been made!"#, escape_discord(server))
            }
        };
        
        let menu = Menu::new(
            (196, 196, 116),
            "Poll ongoing!".to_owned(),
            Some(description)
        );

        let msg = menu.build(&MenuHistory::new("already_polled"), false, false);
        let msg = msg.message().reference_message(&the_poll);
        
        let _ = channel.send_message(shared, msg).await;
        return;
    }

    let mut del = None;

    match &kind {
        PollKind::Install { server, info, .. } => {
            del = Some(DelOnDrop::new(&info.path));

            // What if the mod is already installed?
            let null_history = MenuHistory::new("null");
            let error_msg = match send_command(shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::ListMods(0, 0))) {
                Ok(Response::Mods(mods, _)) => {
                    if mods.iter().any(|modd| &modd.mod_id == &info.mod_id) {
                        // Some(already_installed(&key.mod_id, &key.server).message())
                        // Update the mod instead

                        match send_command(shared, NetCommand::ServerCommand(server.to_owned(), ServerCommand::UpdateMod(info.path.to_string_lossy().to_string(), info.filename.clone()))) {
                            Ok(Response::Ok) => {
                                Some(info_menu(&format!(r#"Mod "{}" updated for "{}"!"#, escape_discord(&info.mod_id), escape_discord(server))).message())
                            }
                            Ok(Response::NoSuchMod) => Some(result_menu(&null_history, false, &format!(r#"Mod "{}" was already installed for "{}", so i tried to update it instead, but the server said it can't be updated because the mod isn't installed... WTF????"#, escape_discord(&info.mod_id), escape_discord(server))).await.message()),
                            Ok(any) => Some(send_unknown(&null_history, &any).await.message()),
                            Err(any) => Some(send_err(&null_history, &any).await.message()),
                        }
                    } else {
                        None
                    }
                }
                Ok(any) => Some(send_unknown(&null_history, &any).await.message()),
                Err(any) => Some(send_err(&null_history, &any).await.message()),
            };
            if let Some(msg) = error_msg {
                let _ = channel.send_message(shared, msg).await;
                return;
            }
        }
        _ => {
        }
    }
    
    match &kind {
        PollKind::Install { info, .. } |
        PollKind::Remove { info, .. } => {
            let mut msg = CreateMessage::new().embed(mod_embed(info));
            if let Some(logo) = &info.logo {
                msg = msg.files([CreateAttachment::bytes(logo.clone(), "logo.png")])
            };
            let _ = channel.send_message(shared, msg).await;
        }
        _ => {}
    };

    
    let poll_title = match &kind {
        PollKind::Install { server, info, .. } => {
            format!(r#"Install "{}" to "{server}"?"#, info.name())
        }
        PollKind::Remove { server, info, .. } => {
            format!(r#"Remove "{}" from "{server}"?"#, info.name())
        }
        PollKind::Restore { server } => {
            format!(r#"Restore backup for "{server}"?"#)
        }
    };
    let poll = CreateMessage::new().poll(CreatePoll::new()
        .question(poll_title)
        .answers([
            CreatePollAnswer::new().emoji("👍".to_string()).text("Yes"),
            CreatePollAnswer::new().emoji("👎".to_string()).text("No")
        ].to_vec())
        .duration(Duration::from_secs(60 * 60 * 24))
    );

    let r: Result<()> = try {
        let poll = channel.send_message(shared, poll.clone()).await.context("Failed to create poll")?;
        let _ = channel.pin(shared.http(), poll.id).await;
        shared.conf.with_config_mut(|config| {
            if config.guild_data[&shared.guild].polls.contains_key(&key) {
                bail!("Poll created twice for same mod_id and same server")
            }
            config.guild_data.get_mut(&shared.guild).unwrap().polls.insert(key, OngoingPoll {
                channel,
                poll: poll.id,
                kind: kind.clone()
            });

            Ok(())
        }).context("Failed to flush config")??;
    };

    if let Err(err) = r {
        println!("Something went wrong");
        dispatch_debug(&err);
        let _ = channel.send_message(shared, result_menu(
            &MenuHistory::new("poll_error"),
            false,
            &format!("Error creating poll: {err}")).await.message()).await;
        return;
    }

    del.map(|del| del.forgive());
}

async fn poll_deleted(shared: &Shared, the_channel: ChannelId, deleted_poll: MessageId) -> bool {
    let (channel, key, poll) = shared.conf.with_config(|conf| {
        let ref guild_data = conf.guild_data[&shared.guild];
        for (key, poll) in guild_data.polls.iter() {
            if poll.channel == the_channel && poll.poll == deleted_poll {
                return (guild_data.notifications, Some(key.clone()), Some(poll.clone()));
            }
        }
        (guild_data.notifications, None, None)
    });

    if let Some(key) = key && let Some(poll) = poll {
        let msg = shared.http().get_message(poll.channel, poll.poll).await;
        if msg.is_ok() {
            return false;
        }

        let _del = if let PollKind::Install { info, .. } = &poll.kind {
            Some(DelOnDrop::new(&info.path))
        } else { None };
        let _ = shared.conf.with_config_mut(|conf| {
            conf.guild_data.get_mut(&shared.guild).unwrap().polls.remove(&key);
        });

        let title = match &poll.kind {
            PollKind::Install { info, server, .. } => {
                format!(r#"Cancelled poll to add "{}" to "{}""#, escape_discord(info.name()), escape_discord(server))
            }
            PollKind::Remove { info, server, .. } => {
                format!(r#"Cancelled poll to remove "{}" from "{}""#, escape_discord(info.name()), escape_discord(server))
            }
            PollKind::Restore { server, .. } => {
                format!(r#"Cancelled poll to restore backup for "{}""#, escape_discord(server))
            }
        };
        let msg = Menu::new((50, 58, 194), title, None)
            .build(&MenuHistory::new("poll_deleted"), false, false)
            .message();
        let _ = channel.send_message(shared, msg).await;
    }
    true
}

async fn check_poll(shared: &Shared, the_channel: ChannelId, the_poll: MessageId) {
    let (channel, key, poll) = shared.conf.with_config(|conf| {
        let ref guild_data = conf.guild_data[&shared.guild];
        for (key, poll) in guild_data.polls.iter() {
            if poll.channel == the_channel && poll.poll == the_poll {
                return (guild_data.notifications, Some(key.clone()), Some(poll.clone()));
            }
        }
        (guild_data.notifications, None, None)
    });

    if let Some(key) = key && let Some(poll) = poll {
        async fn funny_message(http: impl CacheHttp, channel: ChannelId, msg: impl Into<String>) {
            let msg = CreateMessage::new().content(msg);
            let _ = channel.send_message(http.http(), msg).await;
        }

        // Error conditions, error conditions and more error conditions
        let Ok(poll_message) = poll.channel.message(shared.http(), poll.poll).await else {
            funny_message(shared, channel, "You deleted the poll while i was checking the results WHYYYYYYY :(((").await;
            return
        };

        let Some(poll_data) = &poll_message.poll else {
            funny_message(shared, channel, "The poll message somehow doesn't contain a poll. I don't even know how you got here").await;
            return
        };

        let Some(results) = &poll_data.results else {
            funny_message(shared, channel, "Discord API FOR SOME FUCKING REASON didn't include the poll results.").await;
            return
        };

        // *What does everyone mean?? for now, it means every member, excluding the bot itself
        let member_count = shared.guild.members_iter(shared.http()).filter(|member| future::ready(
            if let Ok(member) = member && !member.user.bot {
                true
            } else {
                false
            }
        )).count().await;
        
        let min_count = (member_count / 2) + (member_count % 2);
        
	    let vote_count: usize = results.answer_counts.iter().map(|x| x.count as usize).sum();
        
        // If everyone* voted, and the poll is not yet finalized, end the poll now
        if !results.is_finalized {
            if vote_count >= min_count {
                let _ = poll_message.end_poll(shared.http()).await;
            } else {
                return;
            }
        }

        // No going back from here
        let _ = the_channel.unpin(shared.http(), the_poll).await;
        let del = if let PollKind::Install { info, .. } = &poll.kind {
            Some(DelOnDrop::new(&info.path))
        } else { None };

        let _ = shared.conf.with_config_mut(|conf| {
            conf.guild_data.get_mut(&shared.guild).unwrap().polls.remove(&key);
        });
        let _ = shared.conf.with_config_mut(|conf| {
            conf.guild_data.get_mut(&shared.guild).unwrap().polls.remove(&key);
        });

        let Some(yes_selection) = poll_data.answers.get(0) else {
            funny_message(shared, channel, "The poll doesn't have any answers how :sob:").await;
            return
        };

        let Some(no_selection) = poll_data.answers.get(1) else {
            funny_message(shared, channel, "The poll only has one answer this is getting insane").await;
            return
        };

        let yes_answer = results.answer_counts
            .iter()
            .find(|x| x.id == yes_selection.answer_id)
            .map(|x| x.count)
            .unwrap_or(0);

        let no_answer = results.answer_counts
            .iter()
            .find(|x| x.id == no_selection.answer_id)
            .map(|x| x.count)
            .unwrap_or(0);
        
        match yes_answer.cmp(&no_answer) {
            Ordering::Greater if vote_count >= min_count => {
                let null_history = MenuHistory::new("null");
                match &poll.kind {
                    PollKind::Install { server, info, .. } => {
                        let error_msg = match send_command(shared, NetCommand::ServerCommand(server.clone(), ServerCommand::InstallMod(info.path.to_string_lossy().to_string(), info.filename.clone()))) {
                            Ok(Response::Ok) => {
                                del.map(|del| del.forgive());
                                None
                            },
                            Ok(Response::ModConflict) => {
                                Some(already_installed(&info.mod_id, server).message())
                            }
                            Ok(any) => Some(send_unknown(&null_history, &any).await.message()),
                            Err(any) => Some(send_err(&null_history, &any).await.message()),
                        };

                        if let Some(msg) = error_msg {
                            let _ = channel.send_message(shared, msg).await;
                        } else {
                            reboot_server(shared, server).await;
                        }
                    }
                    PollKind::Remove { server, info, .. } => {
                        let error_msg = match send_command(shared, NetCommand::ServerCommand(server.clone(), ServerCommand::UninstallMod(info.mod_id.clone()))) {
                            Ok(Response::Ok) => {
                                None
                            },
                            Ok(Response::NoSuchMod) => Some(no_such_mod(&info.mod_id, server).message()),
                            Ok(any) => Some(send_unknown(&null_history, &any).await.message()),
                            Err(any) => Some(send_err(&null_history, &any).await.message()),
                        };

                        if let Some(msg) = error_msg {
                            let _ = channel.send_message(shared, msg).await;
                        } else {
                            reboot_server(shared, server).await;
                        }
                    }
                    PollKind::Restore { .. } => {}
                }
            }
            Ordering::Greater => {
                let members = if vote_count == 1 {
                    "member"  
                } else {
                    "members"
                };
                let msg = info_menu(&format!("Poll won, but only {vote_count} {members} voted, expected at least {min_count} votes"))
                    .message()
                    .reference_message(&poll_message);
                let _ = channel.send_message(shared, msg).await;
            }
            _ => {}
        }
    }
}

async fn reboot_server(shared: &Shared, server: impl Into<String> + Clone) {
    let null_history = MenuHistory::new("null");
    // REBOOT THE SERVER
    let msg = match send_command(shared, NetCommand::ServerCommand(server.clone().into(), ServerCommand::Status)) {
        Ok(Response::Status(status)) => {
            if status.status != Status::Idle {
                let _ = send_command(shared, NetCommand::ServerCommand(server.into(), ServerCommand::Reboot));
            }
            None
        }
        Ok(any) => Some(send_unknown(&null_history, &any).await.message()),
        Err(any) => Some(send_err(&null_history, &any).await.message()),
    };

    // Notify of errors happened trying to reboot
    if let Some(msg) = msg {
        let channel = shared.conf.with_config(|conf| conf.guild_data[&shared.guild].notifications);
        let _ = channel.send_message(shared, msg).await;
    }
}

fn mod_embed(info: &ModInfo) -> CreateEmbed {
    let mod_name = info.name.clone().unwrap_or(info.mod_id.clone());

    let mut menu_fields = Vec::new();
    menu_fields.push(("Mod ID".to_owned(), info.mod_id.clone(), true));

    menu_fields.push(("Version".to_owned(), info.version.to_string(), true));

    let mut menu = Menu::new(
        (106, 72, 161),
        mod_name.clone(),
        info.description.clone()
    )
        .fields(menu_fields);

    if info.logo.is_some() {
        menu = menu.logo("attachment://logo.png");
    }

    if let Some(authors) = &info.authors {
        let mut string = String::new();
        let mut first = true;
        for x in authors {
            if first {
                first = false;
                string.push_str(x);
            } else {
                string.push_str(&format!(", {x}"));
            }
        }
        menu = menu.author(&string);
    }

    if let Some(url) = &info.url {
        menu = menu.url(url);
    }

    if let Some(credits) = &info.credits {
        menu = menu.footer(credits);
    }
    menu.0
}