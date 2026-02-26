use actix_web::{delete, get, post, web, HttpResponse};

use aleph_tee::types::VmConfig;

use crate::vm::VmManager;

#[post("/vms")]
pub async fn create_vm(
    manager: web::Data<VmManager>,
    body: web::Json<VmConfig>,
) -> HttpResponse {
    match manager.create_vm(body.into_inner()).await {
        Ok(info) => HttpResponse::Created().json(info),
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": e.to_string(),
        })),
    }
}

#[get("/vms/{id}")]
pub async fn get_vm(
    manager: web::Data<VmManager>,
    path: web::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    match manager.get_vm(&id).await {
        Ok(info) => HttpResponse::Ok().json(info),
        Err(e) => HttpResponse::NotFound().json(serde_json::json!({
            "error": e.to_string(),
        })),
    }
}

#[delete("/vms/{id}")]
pub async fn delete_vm(
    manager: web::Data<VmManager>,
    path: web::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    match manager.delete_vm(&id).await {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(e) => HttpResponse::NotFound().json(serde_json::json!({
            "error": e.to_string(),
        })),
    }
}
