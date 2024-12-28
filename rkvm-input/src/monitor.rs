use crate::device::DeviceSpec;
use crate::interceptor::{Interceptor, OpenError};
use crate::registry::Registry;

use futures::StreamExt;
use inotify::{Inotify, WatchMask};
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};
use std::path::Path;
use tokio::fs;
use tokio::sync::mpsc::{self, Receiver, Sender};

const EVENT_PATH: &str = "/dev/input";

pub struct Monitor {
    receiver: Receiver<Result<Interceptor, Error>>,
}

impl Monitor {
    pub fn new(device_allowlist: Vec<DeviceSpec>) -> Self {
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(monitor(sender, device_allowlist));

        Self { receiver }
    }

    pub async fn read(&mut self) -> Result<Interceptor, Error> {
        self.receiver
            .recv()
            .await
            .ok_or_else(|| Error::new(ErrorKind::BrokenPipe, "Monitor task exited"))?
    }
}

async fn monitor(sender: Sender<Result<Interceptor, Error>>, device_allowlist: Vec<DeviceSpec>) {
    let run = async {
        let registry = Registry::new();

        let mut read_dir = fs::read_dir(EVENT_PATH).await?;

        let inotify = Inotify::init()?;
        inotify.watches().add(EVENT_PATH, WatchMask::CREATE)?;

        // This buffer size should be OK, since we don't expect a lot of devices
        // to be plugged in frequently.
        let mut stream = inotify.into_event_stream([0; 512])?;

        loop {
            let path = match read_dir.next_entry().await? {
                Some(entry) => entry.path(),
                None => match stream.next().await {
                    Some(event) => {
                        let event = event?;
                        let name = match event.name {
                            Some(name) => name,
                            None => continue,
                        };

                        Path::new(EVENT_PATH).join(&name)
                    }
                    None => break,
                },
            };

            if !path
                .file_name()
                .and_then(OsStr::to_str)
                .map_or(false, |name| name.starts_with("event"))
            {
                tracing::debug!("Skipping non event file {:?}", path);
                continue;
            }

            let interceptor = match Interceptor::open(&path, &registry, &device_allowlist).await {
                Ok(interceptor) => interceptor,
                Err(OpenError::Io(err)) => return Err(err),
                Err(OpenError::NotAppliable) => continue,
                Err(OpenError::NotMatchingAllowlist) => continue,
            };

            if sender.send(Ok(interceptor)).await.is_err() {
                return Ok(());
            }
        }

        Ok(())
    };

    tokio::select! {
        result = run => match result {
            Ok(_) => {},
            Err(err) => {
                let _ = sender.send(Err(err)).await;
            }
        },
        _ = sender.closed() => {}
    }
}
