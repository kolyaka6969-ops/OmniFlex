//! ИИ через любой OpenAI-совместимый API. Бесплатные сервисы — готовыми наборами: один щелчок в настройках
//! (Pollinations работает вовсе без ключа).

use std::time::Duration;

use crate::i18n::{tr, trs};
use crate::settings;

pub struct Preset {
    pub name: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    pub transcribe_model: &'static str,
    /// Где взять ключ; «» — ключ не нужен.
    pub key_url: &'static str,
}

pub const PRESETS: [Preset; 5] = [
    Preset { name: "Pollinations", base_url: "https://text.pollinations.ai/openai", model: "openai", transcribe_model: "", key_url: "" },
    Preset { name: "Grok", base_url: "https://api.x.ai/v1", model: "grok-4", transcribe_model: "", key_url: "https://console.x.ai" },
    Preset {
        name: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        model: "gemini-2.0-flash",
        transcribe_model: "",
        key_url: "https://aistudio.google.com/apikey",
    },
    Preset {
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        model: "meta-llama/llama-3.3-70b-instruct:free",
        transcribe_model: "",
        key_url: "https://openrouter.ai/keys",
    },
    Preset { name: "OpenAI", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini", transcribe_model: "whisper-1", key_url: "https://platform.openai.com/api-keys" },
];

fn client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(8))
            .user_agent(format!("OmniFlex/{}", crate::util::VERSION))
            .build()
            .expect("http")
    })
}

fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with(path) {
        base.to_string()
    } else {
        format!("{base}{path}")
    }
}

fn error_text(status: reqwest::StatusCode, body: &str) -> String {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let message = json
        .as_ref()
        .and_then(|j| j["error"]["message"].as_str().or_else(|| j["error"].as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| body.chars().take(200).collect());
    format!("{status}: {message}")
}

/// Вопрос ИИ. Ответ — текст.
pub async fn ask(question: String) -> Result<String, String> {
    let ai = settings::read(|s| s.ai.clone());
    let system = trs(
        "Ты помощник в программе OmniFlex. Отвечай кратко и по делу, на языке вопроса.",
        "You are an assistant in the OmniFlex app. Answer briefly and to the point, in the language of the question.",
    );
    let body = serde_json::json!({
        "model": ai.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": question},
        ],
    });
    let send = |url: String| {
        let mut req = client().post(url).json(&body);
        if !ai.api_key.trim().is_empty() {
            req = req.bearer_auth(ai.api_key.trim());
        }
        req.send()
    };
    let mut response = send(endpoint(&ai.base_url, "/chat/completions")).await.map_err(|e| e.to_string())?;
    // Некоторые сервисы (Pollinations) принимают запрос прямо по базовому адресу
    if response.status() == reqwest::StatusCode::NOT_FOUND || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
        response = send(ai.base_url.trim().to_string()).await.map_err(|e| e.to_string())?;
    }
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(error_text(status, &text));
    }
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| text.chars().take(200).collect::<String>())?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| tr("Пустой ответ", "Empty answer").to_string())
}

/// Речь в текст (Whisper): WAV → текст.
pub async fn transcribe(wav: Vec<u8>) -> Result<String, String> {
    let ai = settings::read(|s| s.ai.clone());
    if ai.transcribe_model.trim().is_empty() {
        return Err(tr("В настройках ИИ не указана модель распознавания речи", "No speech recognition model in AI settings").into());
    }
    let part = reqwest::multipart::Part::bytes(wav).file_name("voice.wav").mime_str("audio/wav").map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new().text("model", ai.transcribe_model.trim().to_string()).part("file", part);
    let mut req = client().post(endpoint(&ai.base_url, "/audio/transcriptions")).multipart(form);
    if !ai.api_key.trim().is_empty() {
        req = req.bearer_auth(ai.api_key.trim());
    }
    let response = req.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(error_text(status, &text));
    }
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(json["text"].as_str().unwrap_or("").trim().to_string())
}

/// Вопрос с острова: строка ввода, затем ответ карточкой.
pub fn command() {
    crate::prompt::ask(tr("Спросить ИИ", "Ask AI"), tr("Вопрос или задача…", "Question or task…"), "", |question| {
        crate::island::card(crate::island::Card::busy(tr("Думаю…", "Thinking…")));
        crate::media::runtime().spawn(async move {
            let result = ask(question.clone()).await;
            crate::shell::post(move || match result {
                Ok(answer) => crate::island::card(crate::island::Card::ai(question, answer)),
                Err(e) => crate::island::card(crate::island::Card::error(tr("ИИ не ответил", "AI did not answer"), e)),
            });
        });
    });
}
