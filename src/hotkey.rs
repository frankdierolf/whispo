use anyhow::{Context, Result};
use rdev::{Event, EventType, Key, grab};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Represents a hotkey combination (modifiers + key)
#[derive(Debug, Clone)]
pub struct Hotkey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
    pub key: Key,
}

impl Hotkey {
    /// Parse a hotkey string like "ctrl+shift+r" into a Hotkey
    pub fn parse(s: &str) -> Result<Self> {
        let lower = s.to_lowercase();
        let parts: Vec<&str> = lower.split('+').map(|p| p.trim()).collect();

        if parts.is_empty() {
            anyhow::bail!("Empty hotkey string");
        }

        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut super_key = false;
        let mut main_key: Option<Key> = None;

        for part in parts {
            match part {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" => alt = true,
                "super" | "meta" | "win" | "cmd" => super_key = true,
                key_str => {
                    main_key = Some(parse_key(key_str)?);
                }
            }
        }

        let key = main_key.context("No main key specified in hotkey")?;

        Ok(Hotkey {
            ctrl,
            shift,
            alt,
            super_key,
            key,
        })
    }
}

/// Parse a single key string into an rdev Key
fn parse_key(s: &str) -> Result<Key> {
    let key = match s {
        "a" => Key::KeyA,
        "b" => Key::KeyB,
        "c" => Key::KeyC,
        "d" => Key::KeyD,
        "e" => Key::KeyE,
        "f" => Key::KeyF,
        "g" => Key::KeyG,
        "h" => Key::KeyH,
        "i" => Key::KeyI,
        "j" => Key::KeyJ,
        "k" => Key::KeyK,
        "l" => Key::KeyL,
        "m" => Key::KeyM,
        "n" => Key::KeyN,
        "o" => Key::KeyO,
        "p" => Key::KeyP,
        "q" => Key::KeyQ,
        "r" => Key::KeyR,
        "s" => Key::KeyS,
        "t" => Key::KeyT,
        "u" => Key::KeyU,
        "v" => Key::KeyV,
        "w" => Key::KeyW,
        "x" => Key::KeyX,
        "y" => Key::KeyY,
        "z" => Key::KeyZ,
        "0" => Key::Num0,
        "1" => Key::Num1,
        "2" => Key::Num2,
        "3" => Key::Num3,
        "4" => Key::Num4,
        "5" => Key::Num5,
        "6" => Key::Num6,
        "7" => Key::Num7,
        "8" => Key::Num8,
        "9" => Key::Num9,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "space" => Key::Space,
        "enter" | "return" => Key::Return,
        "escape" | "esc" => Key::Escape,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        _ => anyhow::bail!("Unknown key: {s}"),
    };
    Ok(key)
}

/// Listen for a hotkey and call the callback when pressed
/// This function blocks and runs until an error occurs
pub fn listen_for_hotkey<F>(hotkey: Hotkey, on_press: F) -> Result<()>
where
    F: Fn() + Send + 'static,
{
    let pressed_keys: Arc<Mutex<HashSet<Key>>> = Arc::new(Mutex::new(HashSet::new()));
    let pressed_keys_clone = pressed_keys.clone();

    let callback = move |event: Event| -> Option<Event> {
        match event.event_type {
            EventType::KeyPress(key) => {
                let mut keys = pressed_keys_clone.lock().unwrap();
                keys.insert(key);

                // Check if hotkey combination is pressed
                let ctrl_ok = !hotkey.ctrl
                    || keys.contains(&Key::ControlLeft)
                    || keys.contains(&Key::ControlRight);
                let shift_ok = !hotkey.shift
                    || keys.contains(&Key::ShiftLeft)
                    || keys.contains(&Key::ShiftRight);
                let alt_ok = !hotkey.alt || keys.contains(&Key::Alt) || keys.contains(&Key::AltGr);
                let super_ok = !hotkey.super_key
                    || keys.contains(&Key::MetaLeft)
                    || keys.contains(&Key::MetaRight);
                let key_ok = keys.contains(&hotkey.key);

                if ctrl_ok && shift_ok && alt_ok && super_ok && key_ok {
                    on_press();
                }
            }
            EventType::KeyRelease(key) => {
                let mut keys = pressed_keys_clone.lock().unwrap();
                keys.remove(&key);
            }
            _ => {}
        }
        // Return Some(event) to pass the event through, None to consume it
        Some(event)
    };

    // This blocks and listens for all keyboard events
    if let Err(e) = grab(callback) {
        anyhow::bail!(
            "Failed to grab keyboard: {e:?}\n\nSetup required:\n  sudo usermod -aG input $USER\n  echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | sudo tee /etc/udev/rules.d/99-uinput.rules\n  sudo udevadm control --reload-rules && sudo udevadm trigger\nThen logout and login again."
        );
    }

    Ok(())
}
