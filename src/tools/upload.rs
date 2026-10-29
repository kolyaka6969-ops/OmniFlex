//! Ссылка на скриншот: загрузка на Imgur или ImgBB со своим ключом. Пока сервис не выбран, ничего не загружается.

use base64::Engine;

use crate::i18n::tr;
use crate::settings::{self, UploadService};

pub fn configured() -> bool {
    settings::read(|s| match s.tools.upload {
        UploadService::None => false,
        UploadService::Imgur => !s.tools.imgur_client_id.trim().is_empty(),
        UploadService::ImgBB => !s.tools.imgbb_key.trim().is_empty(),
    })
}

/// Загружает PNG и возвращает ссылку.
pub async fn upload(png: Vec<u8>) -> Result<String, String> {
    let (service, imgur, imgbb) = settings::read(|s| (s.tools.upload, s.tools.imgur_client_id.clone(), s.tools.imgbb_key.clone()));
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let client = crate::media::slow();
    let response = match service {
        UploadService::None => return Err(tr("Сервис загрузки не выбран в настройках", "No upload service selected in settings").into()),
        UploadService::Imgur => client
            .post("https://api.imgur.com/3/image")
            .header("Authorization", format!("Client-ID {}", imgur.trim()))
            .form(&[("image", data.as_str()), ("type", "base64")])
            .send()
            .await,
        UploadService::ImgBB => client
            .post(format!("https://api.imgbb.com/1/upload?key={}", crate::media::escape(imgbb.trim())))
            .form(&[("image", data.as_str())])
            .send()
            .await,
    };
    let response = response.map_err(|e| e.to_string())?;
    let status = response.status();
    let json: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let link = match service {
        UploadService::Imgur => json["data"]["link"].as_str(),
        _ => json["data"]["url"].as_str(),
    };
    match link {
        Some(l) => Ok(l.to_string()),
        None => Err(format!("{status}: {}", json.get("error").or(json["data"].get("error")).map(|e| e.to_string()).unwrap_or_default())),
    }
}
