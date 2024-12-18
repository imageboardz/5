use actix_files::Files;
use actix_multipart::Multipart;
use actix_web::{web, App, HttpResponse, HttpServer, Error, middleware};
use futures_util::stream::StreamExt;
use html_escape::encode_safe;
use log::error;
use mime_guess::mime;
use std::io::Write;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use image;

#[derive(Clone)]
struct AppState {
    posts: Arc<Mutex<Vec<Post>>>,
}

#[derive(Clone)]
struct Post {
    id: Uuid,
    name: String,
    subject: String,
    body: String,
    image_url: Option<String>,
}

#[derive(Default)]
struct PostData {
    name: String,
    subject: String,
    body: String,
    image_path: Option<String>,
}

const IMAGE_UPLOAD_DIR: &str = "./uploads/images/";

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    std::fs::create_dir_all(IMAGE_UPLOAD_DIR).ok();

    let state = AppState {
        posts: Arc::new(Mutex::new(Vec::new())),
    };

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .wrap(middleware::Logger::default())
            // Root page
            .route("/", web::get().to(homepage))
            // Handle form posts
            .route("/post", web::post().to(handle_post))
            // Serve uploaded images
            .service(Files::new("/uploads/images", "./uploads/images"))
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}

async fn homepage(state: web::Data<AppState>) -> HttpResponse {
    let posts = state.posts.lock().unwrap();

    let mut posts_html = String::new();
    for post in posts.iter().rev() {
        let image_html = if let Some(url) = &post.image_url {
            format!(r#"<div class="image"><img src="{}" alt="image" style="max-width:200px;"></div>"#, encode_html(url))
        } else {
            "".to_string()
        };

        posts_html.push_str(&format!(
            r#"<div class="thread" id="thread_{id}">
<div class="post op" id="op_{id}">
<p class="intro"><span class="subject">{sub}</span> <span class="name">{name}</span></p>
<div class="body">{body}</div>
{image_html}
<hr>
</div>
</div>
"#,
            id = post.id,
            sub = encode_html(&post.subject),
            name = encode_html(&post.name),
            body = encode_html(&post.body),
            image_html = image_html
        ));
    }

    let html = format!(r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>No DB with Images</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
</head>
<body>
<h1>No DB Minimal Board with Images</h1>
<form name="post" enctype="multipart/form-data" action="/post" method="post" style="margin-bottom:20px;">
    <input type="text" name="name" placeholder="Name" required style="display:block;margin-bottom:10px;">
    <input type="text" name="subject" placeholder="Subject" required style="display:block;margin-bottom:10px;">
    <textarea name="body" rows="5" cols="35" placeholder="Comment" required style="display:block;width:300px;height:100px;margin-bottom:10px;"></textarea>
    <input type="file" name="file" accept=".jpg,.jpeg,.png,.gif,.webp" style="display:block;margin-bottom:10px;">
    <input type="submit" value="Post">
</form>
<hr>
{posts}
</body>
</html>"#, posts = posts_html);

    HttpResponse::Ok().content_type("text/html").body(html)
}

async fn handle_post(
    state: web::Data<AppState>,
    mut payload: Multipart,
) -> Result<HttpResponse, Error> {
    let mut post_data = PostData::default();

    while let Some(item) = payload.next().await {
        let mut field = match item {
            Ok(f) => f,
            Err(e) => {
                log_error(&format!("Error processing field: {}", e));
                return Ok(HttpResponse::BadRequest().body("Invalid form data"));
            }
        };

        let disp = field.content_disposition();
        let field_name = disp.get_name().unwrap_or("").to_string();
        let filename = disp.get_filename().map(|s| s.to_string());

        let mut value = Vec::new();
        while let Some(chunk) = field.next().await {
            match chunk {
                Ok(data) => value.extend_from_slice(&data),
                Err(e) => {
                    log_error(&format!("Error reading chunk: {}", e));
                    return Ok(HttpResponse::BadRequest().body("Error reading form data"));
                }
            }
        }

        if field_name == "file" && !value.is_empty() {
            if let Some(fname) = filename {
                let mime_type = mime_guess::from_path(&fname).first_or_octet_stream();
                if mime_type.type_() == mime::IMAGE {
                    if !matches!(mime_type.subtype().as_ref(), "jpeg" | "jpg" | "png" | "gif" | "webp") {
                        return Ok(HttpResponse::BadRequest().body("Unsupported image format"));
                    }

                    let unique_id = Uuid::new_v4().to_string();
                    let extension = mime_type.subtype().as_str();
                    let sanitized_filename = format!("{}.{}", unique_id, extension);
                    let filepath = format!("{}{}", IMAGE_UPLOAD_DIR, sanitized_filename);
                    let filepath_clone = filepath.clone();

                    let mut f = match web::block(move || std::fs::File::create(&filepath)).await {
                        Ok(Ok(file)) => file,
                        _ => {
                            log_error("Failed to create image file");
                            return Ok(HttpResponse::InternalServerError().body("Failed to save image"));
                        }
                    };

                    if let Err(e) = web::block(move || f.write_all(&value)).await {
                        log_error(&format!("Error writing image: {}", e));
                        return Ok(HttpResponse::InternalServerError().body("Failed to write image"));
                    }

                    if image::open(&filepath_clone).is_err() {
                        std::fs::remove_file(&filepath_clone).ok();
                        return Ok(HttpResponse::BadRequest().body("Invalid image file"));
                    }

                    post_data.image_path = Some(format!("/uploads/images/{}", sanitized_filename));
                }
            }
        } else {
            let value_str = String::from_utf8_lossy(&value).to_string();
            match field_name.as_str() {
                "name" => post_data.name = value_str.trim().to_string(),
                "subject" => post_data.subject = value_str.trim().to_string(),
                "body" => post_data.body = value_str.trim().to_string(),
                _ => {}
            }
        }
    }

    if post_data.name.is_empty() || post_data.subject.is_empty() || post_data.body.is_empty() {
        return Ok(HttpResponse::BadRequest().body("Name, Subject, and Body are required"));
    }

    let post = Post {
        id: Uuid::new_v4(),
        name: post_data.name,
        subject: post_data.subject,
        body: post_data.body,
        image_url: post_data.image_path,
    };

    {
        let mut posts = state.posts.lock().unwrap();
        posts.push(post);
    }

    Ok(HttpResponse::SeeOther().append_header(("Location", "/")).finish())
}

fn encode_html(input: &str) -> String {
    encode_safe(input).to_string()
}

fn log_error(msg: &str) {
    error!("{}", msg);
}
