use std::{
    fmt::format, future::Future, io::{self, ErrorKind}, str::FromStr, sync::Arc, time::{Duration, SystemTime}
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Mutex;
use anyhow::{anyhow, Result};
use async_scoped::TokioScope;
use base64::{engine::general_purpose, Engine};
use ende::{io::{Slice, VecStream}, Decode, Encode, Encoder};
use is_url::is_url;
use once_cell::sync::Lazy;
use parse_display::Display;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use serenity::{
    all::{
        ActionRowComponent, ButtonStyle, CacheHttp, CommandInteraction, ComponentInteractionDataKind, Context, CreateActionRow, CreateButton, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateInputText, CreateInteractionResponse, CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage, CreateModal, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, EditInteractionResponse, EditMessage, EventHandler, GatewayIntents, InputTextStyle, Interaction, InteractionId, Message, MessageBuilder, Ready
    },
    async_trait, Client,
};
use serenity::all::{ChannelId, MessageId};
use serenity_commands::Commands;
use sha2::digest::Output;
use tokio::{join, sync::RwLock};
use url::{Host, Url};
use yapper::{dispatch_debug, escape_discord, NetCommand, Notification, pretty_status, Response, ServerCommand, ServerStatus, Status};
use yapper::conf::Config;
use crate::{comm::{send_cmd, send_command}, conf::{MCAYB}};

const TOKEN: &str = include_str!("../discord.token");

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

    pub fn fields(mut self, fields: Vec<(String, String, bool)>) -> Self {
        self.0 = self.0.fields(fields);
        self
    }

    pub fn buttons(mut self, buttons: Vec<CreateButton>) -> Self {
        self.1 = buttons;
        self
    }

    pub fn to_create_interaction(self, current: &MenuHistory, back: bool, refresh: bool) -> CreateInteractionResponseMessage {
        let mut buttons = self.1;
        if refresh {
            buttons.insert(0, CreateButton::new(current.to_string()).label("Refresh").emoji('🔄').style(ButtonStyle::Primary))
        }
        if back && let Some(previous) = current.exit_page() {
            buttons.insert(0, CreateButton::new(previous.to_string()).label("Back").style(ButtonStyle::Secondary))
        }
    

        let response = CreateInteractionResponseMessage::new()
            .embed(self.0);

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
}

async fn send_err(h: &MenuHistory, error: &anyhow::Error) -> CreateInteractionResponseMessage {
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

async fn send_unknown(h: &MenuHistory, resp: &Response) -> CreateInteractionResponseMessage {
    dispatch_debug(anyhow!("Unexpected response: {resp}"));

    result_menu(h, false, "Server sent an invalid response. This is a bug").await
}

async fn unknown_server(h: &MenuHistory, server: &str) -> CreateInteractionResponseMessage {
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
    /// Registers this channel as a receiver for server updates
    UpdateMe,
    /// Does the opposite of UpdateMe
    ForgetMe,
    /// Test
    Test,
}

impl AllCommands {
    async fn run(self, conf: &Config<MCAYB>, ctx: &Context, interaction: &CommandInteraction) -> CreateInteractionResponseMessage {
        let null_menu = MenuHistory::new("null");
        match self {
            AllCommands::Dashboard => dashboard_menu(&MenuHistory::new("dashboard")).await,
            AllCommands::Servers => match send_command(NetCommand::ListServers) {
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
                Ok(any) => send_unknown(&null_menu, &any).await,
                Err(any) => send_err(&null_menu, &any).await,
            },
            AllCommands::Status { server } => match send_command(NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Status,
            )) {
                Ok(Response::Status(status)) => result_menu(&null_menu, true, &format!("Server is {}", pretty_status(status.status))).await,
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await,
                Ok(any) => send_unknown(&null_menu, &any).await,
                Err(any) => send_err(&null_menu, &any).await,
            },
            AllCommands::Start { server } => match send_command(NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Start,
            )) {
                Ok(Response::Ok) => result_menu(&null_menu, true, "Server started!").await,
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await,
                Ok(any) => send_unknown(&null_menu, &any).await,
                Err(any) => send_err(&null_menu, &any).await,
            },
            AllCommands::Stop { server } => match send_command(NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Stop,
            )) {
                Ok(Response::Ok) => result_menu(&null_menu, true, "Server stopped.").await,
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await,
                Ok(any) => send_unknown(&null_menu, &any).await,
                Err(any) => send_err(&null_menu, &any).await,
            },
            AllCommands::Reboot { server } => match send_command(NetCommand::ServerCommand(
                server.clone(),
                ServerCommand::Reboot,
            )) {
                Ok(Response::Ok) => result_menu(&null_menu, true, "Server is rebooting!").await,
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await,
                Ok(any) => send_unknown(&null_menu, &any).await,
                Err(any) => send_err(&null_menu, &any).await,
            },
            AllCommands::Command { server, command } => match send_command(
                NetCommand::ServerCommand(server.clone(), ServerCommand::Console(command.clone())),
            ) {
                Ok(Response::CommandOutput(output)) => CreateInteractionResponseMessage::new().content(format!(
                        "`/{}` => `{}`",
                        escape_discord(&command),
                        escape_discord(&output)
                    )),
                Ok(Response::UnknownServer) => unknown_server(&null_menu, &server).await,
                Ok(any) => send_unknown(&null_menu, &any).await,
                Err(any) => send_err(&null_menu, &any).await,
            },
            AllCommands::UpdateMe => {

                let channel_id = interaction.channel_id;
                
                let result = conf.with_config_mut(|conf| {
                    if !conf.update_receivers.contains(&channel_id) {
                        conf.update_receivers.push(channel_id);
                    }
                });

                match result {
                    Ok(_) => result_menu(&null_menu, true, "Success!").await,
                    Err(error) => {
                        dispatch_debug(error);
                        result_menu(&null_menu, true, "Internal error. This is a bug!").await
                    }
                }
            },
            AllCommands::ForgetMe => {
                let channel_id = interaction.channel_id;
                
                let result = conf.with_config_mut(|conf| {
                    conf.update_receivers.retain(|id| id != &channel_id);
                });

                match result {
                    Ok(_) => result_menu(&null_menu, true, "Success!").await,
                    Err(error) => {
                        dispatch_debug(error);
                        result_menu(&null_menu, true, "Internal error. This is a bug!").await
                    }
                }
            },
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
        }
    }
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
}

impl From<&str> for MenuHistory {
    fn from(value: &str) -> Self {
        let this: Result<Self> = try {
            let engine = general_purpose::URL_SAFE;
            let string = engine.decode(value)?;
            let mut decoder = Encoder::new(Slice::new(&string), ende::Context::new());
            MenuHistory::decode(&mut decoder)?
        };
        this.unwrap_or(MenuHistory::new("broken"))
    }
}

impl core::fmt::Display for MenuHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut encoder = Encoder::new(VecStream::new(Vec::new(), 0), ende::Context::new());
        self.encode(&mut encoder).unwrap();
        let data = encoder.finish().0.into_inner();
        let engine = general_purpose::URL_SAFE;
        let mut encoded = String::new();
        engine.encode_string(data, &mut encoded);
        if encoded.len() > 100 {
            let broken = MenuHistory::new("broken");
            println!("Tried to encode {self:?} (too long!). Encoding {broken:?} instead");
            return broken.fmt(f);
        }
        write!(f, "{}", encoded)
    }
}

async fn dashboard_menu(history: &MenuHistory) -> CreateInteractionResponseMessage {
    match send_command(NetCommand::ListServers) {
        Ok(Response::List(statuses)) => {
            let mut fields: Vec<(String, String, bool)> = Vec::with_capacity(statuses.len());
            let mut buttons: Vec<CreateButton> = Vec::with_capacity(statuses.len());

            for status in statuses {
                fields.push((
                    format!("> `{}`", escape_discord(&status.name)),
                    pretty_status(status.status),
                    true
                ));

                let page = MenuUrl::page("menu", &[&status.name]);
                let page = history.enter_page(page);

                buttons.push(
                    CreateButton::new(page.to_string())
                        .label(status.name)
                        .style(ButtonStyle::Success)
                )
            }

            Menu::new((3, 227, 252), "Server dashboard".to_owned(), Some("Pick which server you want to interact with".to_owned()))
                .fields(fields)
                .buttons(buttons)
                .to_create_interaction(history, true, true)
        }
        Ok(any) => send_unknown(history, &any).await,
        Err(any) => send_err(history, &any).await,
    }
}

async fn server_menu(history: &MenuHistory, server: &str) -> CreateInteractionResponseMessage {
    match send_command(NetCommand::ServerCommand(server.to_owned(), ServerCommand::Status)) {
        Ok(Response::Status(status)) => {
            let mut buttons: Vec<CreateButton> = Vec::with_capacity(3);
            match status.status {
                Status::Idle | Status::Stopping => {
                    let action = MenuUrl::action("start", &[server]);
                    let action = history.enter_page(action);

                    buttons.push(CreateButton::new(action.to_string()).label("Start").emoji('▶').style(ButtonStyle::Success));
                }
                Status::Running | Status::Starting => {
                    let action = MenuUrl::action("stop", &[server]);
                    let action = history.enter_page(action);

                    buttons.push(CreateButton::new(action.to_string()).label("Stop").emoji('🛑').style(ButtonStyle::Danger));
                }
                _ => {}
            }
            let action = MenuUrl::action("reboot", &[server]);
            let action = history.enter_page(action);
            
            buttons.push(CreateButton::new(action.to_string()).label("Reboot").emoji('🔄').style(ButtonStyle::Success));

            if let Status::Running = status.status {
                let action = MenuUrl::action("command", &[server]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_string()).label("Command").emoji('🔑').style(ButtonStyle::Secondary));
            }
            if let Status::Idle = status.status {
                let action = MenuUrl::action("backup", &[server]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_string()).label("Backup").emoji('💾').style(ButtonStyle::Danger));

                let action = MenuUrl::action("restore", &[server]);
                let action = history.enter_page(action);

                buttons.push(CreateButton::new(action.to_string()).label("Restore").emoji('↩').style(ButtonStyle::Danger));
            }

            Menu::new((245, 167, 66), server.to_owned(), None)
                .fields([
                    ("Status".to_string(), format!("{}", pretty_status(status.status)), false),
                    ("Path".to_string(), format!("{}", escape_discord(status.path)), false),
                ].to_vec())
                .buttons(buttons)
                .to_create_interaction(history, true, true)
        }
        Ok(Response::UnknownServer) => unknown_server(history, &server).await,
        Ok(any) => send_unknown(history, &any).await,
        Err(any) => send_err(history, &any).await,
    }
}


async fn result_menu(history: &MenuHistory, success: bool, message: &str) -> CreateInteractionResponseMessage {
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
        .to_create_interaction(history, true, false)
}

async fn wtf_bad_bot(history: &MenuHistory) -> CreateInteractionResponseMessage {
    Menu::new((0, 0, 0), "Wtf bad bot".to_string(), Some(format!("{history:?}")))
        .to_create_interaction(history, false, false)
}

struct Handler {
    conf: Config<MCAYB>
}

impl Handler {
    pub fn new(conf: Config<MCAYB>) -> Self {
        Self { conf }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, new_message: Message) {
        let Some(guild_id) = new_message.guild_id else { return };
        if guild_id != 1127043416635736155 {
            return;
        }

        for p in new_message.content.split(" ") {
            if let Ok(url) = Url::parse(p) && url.scheme() == "https" &&
                let Some(host) = url.host() &&
                let Host::Domain("www.curseforge.com") = host &&
                let Some(mut segments) = url.path_segments() &&
                let Some("minecraft") = segments.next() &&
                let Some("mc-mods") = segments.next() &&
                let Some(mod_name) = segments.next()
            {
                let _ = new_message.reply(&ctx, format!("You posted a link to a mc mod: {mod_name}")).await;
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
        if guild_id.get() != 1127043416635736155 {
            return;
        }

        const CONFIRM: &str = "yes, i confirm";
        const CONFIRM_BACKUP: &str = "the last backup will be lost";
        const CONFIRM_RESTORE: &str = "all changes will be lost";

        match interaction {
            Interaction::Command(command) => {
                let command_data = AllCommands::from_command_data(&command.data).unwrap();
                let _ = command
                    .create_response(&ctx.http, CreateInteractionResponse::Message(command_data.run(&self.conf, &ctx, &command).await))
                    .await;
            }
            Interaction::Component(mut comp) => {
                let id = &comp.data.custom_id;
                let h = MenuHistory::from(id as &str);

                let response = match h.current.kind {
                    MenuUrlKind::Page => {
                        match &h.current.url as &str {
                            "dashboard" => {
                                CreateInteractionResponse::UpdateMessage(dashboard_menu(&h).await)
                            }
                            "menu" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(server_menu(&h, server).await)
                            }
                            _ => {
                                CreateInteractionResponse::UpdateMessage(wtf_bad_bot(&h).await)
                            }
                        }
                    }
                    MenuUrlKind::Action => {
                        match &h.current.url as &str {
                            "start" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(
                                    match send_command(NetCommand::ServerCommand(server.to_owned(), ServerCommand::Start)) {
                                        Ok(Response::Ok) => result_menu(&h, true, "Server started!").await,
                                        Ok(Response::UnknownServer) => unknown_server(&h, &server).await,
                                        Ok(any) => send_unknown(&h, &any).await,
                                        Err(any) => send_err(&h, &any).await,
                                })
                            }
                            "stop" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(
                                    match send_command(NetCommand::ServerCommand(server.to_owned(), ServerCommand::Stop)) {
                                        Ok(Response::Ok) => result_menu(&h, true, "Server stopped.").await,
                                        Ok(Response::UnknownServer) => unknown_server(&h, &server).await,
                                        Ok(any) => send_unknown(&h, &any).await,
                                        Err(any) => send_err(&h, &any).await,
                                })
                            }
                            "reboot" => {
                                let server = &h.current.arguments[0];
                                CreateInteractionResponse::UpdateMessage(
                                    match send_command(NetCommand::ServerCommand(server.to_owned(), ServerCommand::Reboot)) {
                                        Ok(Response::Ok) => result_menu(&h, true, "Server is rebooting!").await,
                                        Ok(Response::UnknownServer) => unknown_server(&h, &server).await,
                                        Ok(any) => send_unknown(&h, &any).await,
                                        Err(any) => send_err(&h, &any).await,
                                })
                            }
                            "command" => {
                                let server = &h.current.arguments[0];
                                
                                CreateInteractionResponse::Modal(
                                    CreateModal::new(h.to_string(), format!("Input command for `{}`", escape_discord(server)))
                                        .components([
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, "command", "command_field"))
                                        ].to_vec())
                                )
                            }
                            "backup" => {
                                let server = &h.current.arguments[0];

                                CreateInteractionResponse::Modal(
                                    CreateModal::new(h.to_string(), format!("Backup `{}`?", escape_discord(server)))
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
                                    CreateModal::new(h.to_string(), format!("Restore `{}`?", escape_discord(server)))
                                        .components([
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, format!(r#"type "{CONFIRM}""#), "confirm_field1")),
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, format!(r#"type "{CONFIRM_RESTORE}""#), "confirm_field2")),
                                            CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, "type the name of the server", "confirm_field3"))
                                        ].to_vec())
                                )
                            }
                            _ => {
                                CreateInteractionResponse::UpdateMessage(wtf_bad_bot(&h).await)
                            }
                        }
                    }
                };

                let _ = comp.create_response(&ctx.http, response).await;
            }
            Interaction::Modal(modal) => {
                let id = &modal.data.custom_id;
                let h = MenuHistory::from(id as &str);

                let response = match h.current.kind {
                    MenuUrlKind::Page => {
                        match &h.current.url as &str {
                            _ => {
                                CreateInteractionResponse::Message(wtf_bad_bot(&h).await)
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
                                    match send_command(NetCommand::ServerCommand(server.to_owned(), ServerCommand::Console(command.clone()))) {
                                        Ok(Response::CommandOutput(output)) => result_menu(&h, true, &output).await,
                                        Ok(Response::UnknownServer) => unknown_server(&h, &server).await,
                                        Ok(any) => send_unknown(&h, &any).await,
                                        Err(any) => send_err(&h, &any).await,
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
                                        match send_command(NetCommand::ServerCommand(server.clone(), ServerCommand::Backup)) {
                                            Ok(Response::Ok) => result_menu(&h, true, "Backing up!").await,
                                            Ok(Response::UnknownServer) => unknown_server(&h, &server).await,
                                            Ok(any) => send_unknown(&h, &any).await,
                                            Err(any) => send_err(&h, &any).await,
                                        }
                                    } else {
                                        result_menu(&h, false, "Please properly confirm this action!").await
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
                                        match send_command(NetCommand::ServerCommand(server.clone(), ServerCommand::Restore)) {
                                            Ok(Response::Ok) => result_menu(&h, true, "Restoring backup!").await,
                                            Ok(Response::UnknownServer) => unknown_server(&h, &server).await,
                                            Ok(Response::NoBackup) => result_menu(&h, false, "No backup exists!").await,
                                            Ok(any) => send_unknown(&h, &any).await,
                                            Err(any) => send_err(&h, &any).await,
                                        }
                                    } else {
                                        result_menu(&h, false, "Please properly confirm this action!").await
                                    }
                                )
                            }
                            _ => {
                                CreateInteractionResponse::UpdateMessage(wtf_bad_bot(&h).await)
                            }
                        }
                    }
                };
                
                let _ = modal.create_response(&ctx.http, response).await;
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
            let msg = EditMessage::new().content(msg);
            let _ = channel.edit_message(cache, message, msg).await;
        } else {
            let msg = CreateMessage::new().content(msg);
            let Ok(message) = channel.send_message(cache, msg).await else { return };
            self.channel_messages.insert(channel, message.id);
        }
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

static MSGS: Lazy<Mutex<Notifs>> = Lazy::new(|| Mutex::new(Notifs::new(&["backup", "restore", "install_mod", "uninstall_mod"])));

pub async fn init(conf: Config<MCAYB>) -> Result<()> {
    use anyhow::Context;

    // Log into discord bot
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::DIRECT_MESSAGES;

    let mut client = Client::builder(TOKEN, intents)
        .event_handler(Handler::new(conf.clone()))
        .await
        .context("Failed to log into discord bot")?;

    
    let http = client.http.clone();

    let handle = tokio::spawn(async move {
        if let Err(err) = client.start().await {
            dispatch_debug(&err);
        };
    });

    let mut last_status = conf.with_config(|x| x.last_status.clone());
    let mut last = SystemTime::now();

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

    loop {
        let now = SystemTime::now();
        if now.duration_since(last).unwrap() > Duration::from_secs(2) {
            last = now;

            static REC: Mutex<Vec<ChannelId>> = Mutex::new(Vec::new());

            // Detect notifications the server can't give us
            match send_command(NetCommand::ListServers) {
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
                        let _ = conf.with_config_mut(|conf| {
                            conf.last_status = new_status;
                        });
                    }


                    for event in events {
                        let msg = CreateMessage::new()
                            .content(event.to_string());

                        let mut rec = REC.lock().unwrap();
                        conf.with_config(|config| {
                            if &config.update_receivers != &*rec {
                                *rec = config.update_receivers.clone();
                            }
                        });

                        for channel in rec.iter() {
                            let _ = channel.send_message(&http, msg.clone()).await;
                        }
                    }
                }
                _ => {}
            }

            // Receive normal notifications
            match send_command(NetCommand::Notifications) {
                Ok(Response::Notifications(notifs)) => {
                    for notif in notifs {
                        let mut rec = REC.lock().unwrap();
                        conf.with_config(|config| {
                            if &config.update_receivers != &*rec {
                                *rec = config.update_receivers.clone();
                            }
                        });
                        
                        match &notif {
                            Notification::BackupProgress(server, _, _) => {
                                let mut msgs = MSGS.lock().unwrap();
                                for channel in rec.iter() {
                                    msgs.get("restore").get(server).send_msg(&http, channel, notif.to_string()).await;
                                }
                            }
                            Notification::RestoreProgress(server, _, _) => {
                                let mut msgs = MSGS.lock().unwrap();
                                for channel in rec.iter() {
                                    msgs.get("backup").get(server).send_msg(&http, channel, notif.to_string()).await;
                                }
                            }
                            notif => {
                                let msg = CreateMessage::new()
                                    .content(notif.to_string());

                                for channel in rec.iter() {
                                    let _ = channel.send_message(&http, msg.clone()).await;
                                }
                            }
                        }
                        
                        // Reset messages statuses
                        if let Notification::StatusChanged(server, _, new) = &notif {
                            if *new != Status::BackingUp {
                                let mut msgs = MSGS.lock().unwrap();
                                msgs.get("backup").get(server).clear();
                            }
                            if *new != Status::Restoring {
                                let mut msgs = MSGS.lock().unwrap();
                                msgs.get("restore").get(server).clear();
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}
