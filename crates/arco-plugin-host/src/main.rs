#[cfg(windows)]
use arco_core::plugin::{Interface, Operation};
use arco_core::plugin::{MAX_REQUEST, PROTOCOL_VERSION, Reply, Request, Row};
use std::io::{self, BufRead, Read, Write};

#[cfg(windows)]
mod native;

fn main() -> io::Result<()> {
    let request = request();
    #[cfg(windows)]
    if let Ok(request) = &request
        && matches!(request.interface, Interface::Wlx)
        && matches!(
            request.operation,
            Operation::Run | Operation::WlxEmbedded { .. }
        )
    {
        let mut ready = false;
        let result = native::view(request, |window| {
            let rows = window
                .into_iter()
                .map(|hwnd| Row {
                    name: "hwnd".into(),
                    value: hwnd.to_string(),
                    ..Default::default()
                })
                .collect();
            send_reply(Ok(rows))?;
            ready = true;
            Ok(())
        });
        return if ready {
            result
        } else {
            send_reply(result.map(|()| Vec::new()))
        };
    }
    send_reply(request.and_then(inspect))
}

fn send_reply(result: io::Result<Vec<Row>>) -> io::Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(
        &mut output,
        &Reply {
            version: PROTOCOL_VERSION,
            result: result.map_err(|error| error.to_string()),
        },
    )
    .map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn request() -> io::Result<Request> {
    let mut data = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_REQUEST + 1)
        .read_until(b'\n', &mut data)?;
    if data.len() as u64 > MAX_REQUEST {
        return Err(io::Error::other("Plugin-Anfrage ist zu groß"));
    }
    let request: Request = serde_json::from_slice(&data).map_err(io::Error::other)?;
    if request.version != PROTOCOL_VERSION || !request.plugin.is_absolute() {
        return Err(io::Error::other("ungültige Plugin-Anfrage"));
    }
    Ok(request)
}

#[cfg(windows)]
fn inspect(request: Request) -> io::Result<Vec<Row>> {
    native::inspect(request)
}

#[cfg(not(windows))]
fn inspect(_: Request) -> io::Result<Vec<Row>> {
    Err(io::Error::other("der Plugin-Host benötigt Windows"))
}
