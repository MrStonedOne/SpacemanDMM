//! Debug adapter protocol implementation for DreamSeeker.
//!
//! * https://microsoft.github.io/debug-adapter-protocol/

mod dap_types;

use std::error::Error;
use std::process::{Command, Stdio, Child};

use io;
use self::dap_types::*;

pub fn debugger_main<I: Iterator<Item=String>>(mut args: I) {
    eprintln!("acting as debug adapter");
    let mut dreamseeker_exe = None;

    while let Some(arg) = args.next() {
        if arg == "--dreamseeker-exe" {
            dreamseeker_exe = Some(args.next().expect("must specify a value for --dreamseeker-exe"));
        } else {
            panic!("unknown argument {:?}", arg);
        }
    }

    let dreamseeker_exe = dreamseeker_exe.expect("must provide argument `--dreamseeker-exe path/to/dreamseeker.exe`");
    eprintln!("dreamseeker: {}", dreamseeker_exe);

    let mut debugger = Debugger::new(dreamseeker_exe);
    io::run_forever(|message| debugger.handle_input(message));
}

struct Debugger {
    dreamseeker_exe: String,
    child: Option<Child>,
    // TODO: separate field from `child` for attached debugger.

    seq: i64,
    client_caps: ClientCaps,
}

impl Debugger {
    fn new(dreamseeker_exe: String) -> Self {
        Debugger {
            dreamseeker_exe,
            child: None,

            seq: 0,
            client_caps: Default::default(),
        }
    }

    fn handle_input(&mut self, message: &str) {
        // TODO: error handling
        self.handle_input_inner(message).expect("error in handle_input");
    }

    fn handle_input_inner(&mut self, message: &str) -> Result<(), Box<dyn Error>> {
        let protocol_message = serde_json::from_str::<ProtocolMessage>(message)?;
        match protocol_message.type_.as_str() {
            RequestMessage::TYPE => {
                let request = serde_json::from_str::<RequestMessage>(message)?;
                let request_seq = request.protocol_message.seq;
                let command = request.command.clone();

                let handled = self.handle_request(request);
                let response = ResponseMessage {
                    protocol_message: ProtocolMessage {
                        seq: self.next_seq(),
                        type_: ResponseMessage::TYPE.to_owned(),
                    },
                    request_seq,
                    command,
                    success: handled.is_ok(),
                    message: handled.as_ref().err().map(|err| err.to_string()),
                    body: match handled {
                        Ok(result) => Some(result),
                        Err(_) => None,
                    }
                };
                io::write(serde_json::to_string(&response).expect("response encode error"))
            }
            other => return Err(format!("unknown `type` field {:?}", other).into())
        }
        Ok(())
    }

    fn issue_event<E: Event>(&mut self, event: E) {
        let body = serde_json::to_value(event).expect("event body encode error");
        let message = EventMessage {
            protocol_message: ProtocolMessage {
                seq: self.next_seq(),
                type_: EventMessage::TYPE.to_owned(),
            },
            event: E::EVENT.to_owned(),
            body: Some(body),
        };
        io::write(serde_json::to_string(&message).expect("event encode error"))
    }

    fn next_seq(&mut self) -> i64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }
}

handle_request! {
    on Initialize(&mut self, params) {
        // Initialize client caps from request
        self.client_caps = ClientCaps::parse(&params);
        let debug = format!("{:?}", self.client_caps);
        if let (Some(start), Some(end)) = (debug.find('{'), debug.rfind('}')) {
            eprintln!("client capabilities: {}", &debug[start + 2..end - 1]);
        } else {
            eprintln!("client capabilities: {}", debug);
        }

        // ... clientID, clientName, adapterID, locale, pathFormat

        // Tell the client our caps
        Some(Capabilities {
            supportTerminateDebuggee: Some(true),
            .. Default::default()
        })
    }

    on LaunchVsc(&mut self, params) {
        let _debug = !params.base.noDebug.unwrap_or(false);

        let child = Command::new(&self.dreamseeker_exe)
            .arg(&params.dmb)
            .arg("-trusted")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        self.child = Some(child);
    }

    on Disconnect(&mut self, params) {
        // TODO: `false` if `attach` was used instead of `launch`.
        let default_terminate = true;
        let terminate = params.terminateDebuggee.unwrap_or(default_terminate);

        if let Some(mut child) = self.child.take() {
            if terminate {
                child.kill()?;
                let status = child.wait()?;
                let code = status.code().unwrap_or(-1);
                self.issue_event(ExitedEvent {
                    exitCode: code as i64,
                });
            } else {
                // On some OSes, a wait() is necessary to free resources.
                std::thread::Builder::new()
                    .name("detached debuggee wait() thread".to_owned())
                    .spawn(move || {
                        let _ = child.wait();
                    })?;
                self.issue_event(TerminatedEvent::default());
            }
        }
    }
}

#[derive(Default, Debug)]
struct ClientCaps {
    lines_start_at_1: bool,
    columns_start_at_1: bool,
    variable_type: bool,
    variable_paging: bool,
    run_in_terminal: bool,
    memory_references: bool,
}

impl ClientCaps {
    fn parse(params: &InitializeRequestArguments) -> ClientCaps {
        ClientCaps {
            lines_start_at_1: params.linesStartAt1.unwrap_or(true),
            columns_start_at_1: params.columnsStartAt1.unwrap_or(true),

            variable_type: params.supportsVariableType.unwrap_or(false),
            variable_paging: params.supportsVariablePaging.unwrap_or(false),
            run_in_terminal: params.supportsRunInTerminalRequest.unwrap_or(false),
            memory_references: params.supportsMemoryReferences.unwrap_or(false),
        }
    }
}

// ----------------------------------------------------------------------------
// Implementation-specific DAP extensions.

enum LaunchVsc {}

impl Request for LaunchVsc {
    type Params = LaunchRequestArgumentsVsc;
    type Result = ();
    const COMMAND: &'static str = Launch::COMMAND;
}

#[derive(Deserialize)]
pub struct LaunchRequestArgumentsVsc {
    #[serde(flatten)]
    base: LaunchRequestArguments,

    // provided by vscode
    dmb: String,

    // other keys: __sessionId, name, preLaunchTask, request, type
}