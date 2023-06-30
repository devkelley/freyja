// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
// SPDX-License-Identifier: MIT

mod config;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::{fs, io, net::SocketAddr, path::Path, thread, time::Duration};

use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{extract, extract::State, Json, Router, Server};
use env_logger::Target;
use log::{debug, error, info, warn, LevelFilter};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::{mpsc, mpsc::UnboundedSender};

use config::{ConfigItem, Settings, CONFIG_FILE};
use dts_contracts::digital_twin_adapter::{
    EntityValueRequest, EntityValueResponse, GetDigitalTwinProviderResponse,
};
use dts_contracts::entity::Entity;
use mock_digital_twin::{ENTITY_GET_VALUE_PATH, ENTITY_PATH, ENTITY_SUBSCRIBE_PATH};

const GET_OPERATION: &str = "Get";
const SUBSCRIBE_OPERATION: &str = "Subscribe";

/// Stores the state of active entities, subscribers, and relays responses
/// for getting/subscribing to an entity.
struct DigitalTwinAdapterState {
    count: u8,
    entities: Vec<(ConfigItem, u8)>,
    subscriptions: HashMap<String, HashSet<String>>,
    response_channel_sender: UnboundedSender<(String, EntityValueResponse)>,
}

/// Used for deserializing a query parameter for /entity?id=...
#[derive(Deserialize)]
struct EntityQuery {
    id: String,
}

macro_rules! response {
    ($status_code:ident) => {
        (axum::http::StatusCode::$status_code, axum::Json("")).into_response()
    };
    ($status_code:ident, $body:expr) => {
        (axum::http::StatusCode::$status_code, axum::Json($body)).into_response()
    };
}

macro_rules! ok {
    () => {
        response!(OK)
    };
    ($body:expr) => {
        response!(OK, $body)
    };
}

macro_rules! not_found {
    () => {
        response!(NOT_FOUND)
    };
    ($body:expr) => {
        response!(NOT_FOUND, $body)
    };
}

macro_rules! server_error {
    () => {
        response!(INTERNAL_SERVER_ERROR)
    };
    ($body:expr) => {
        response!(INTERNAL_SERVER_ERROR, $body)
    };
}

/// Starts the following threads and tasks:
/// - A thread which listens for input from the command window
/// - A task which handles async get responses
/// - A task which handles publishing to subscribers
/// - An HTTP listener to accept incoming requests
#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .filter(None, LevelFilter::Info)
        .target(Target::Stdout)
        .init();

    let settings_content =
        fs::read_to_string(Path::new(env!("OUT_DIR")).join(CONFIG_FILE)).unwrap();
    let settings: Settings = serde_json::from_str(settings_content.as_str()).unwrap();
    let config_items = settings.config_items;

    let (sender, mut receiver) = mpsc::unbounded_channel::<(String, EntityValueResponse)>();

    let state = Arc::new(Mutex::new(DigitalTwinAdapterState {
        count: 0,
        entities: config_items.iter().map(|c| (c.clone(), 0)).collect(),
        subscriptions: config_items
            .iter()
            .map(|c| (c.value.entity.id.clone(), HashSet::new()))
            .collect(),
        response_channel_sender: sender,
    }));

    let console_listener_state = state.clone();
    let subscribe_loop_state = state.clone();

    {
        let initial_state = state.lock().unwrap();
        info!(
            "Initial entity list: {:?}",
            get_active_entity_names(&initial_state)
        );
    }

    // stdin setup
    thread::spawn(move || -> std::io::Result<usize> {
        let mut buffer = String::new();
        loop {
            io::stdin().read_line(&mut buffer)?;

            let mut state = console_listener_state.lock().unwrap();
            state.count += 1;
            info!(
                "New count: {}. Active entities {:?}",
                state.count,
                get_active_entity_names(&state)
            );
        }
    });

    // Get responder setup
    tokio::spawn(async move {
        let client = Client::new();
        loop {
            let message = receiver.recv().await;
            if message.is_none() {
                debug!("Channel is closed, aborting get responder...");
                break;
            }

            let request = message.unwrap();
            info!("Handling GET for request {request:?}...");
            let (callback_uri_for_signals, response_to_send) = request.clone();

            let send_result = client
                .post(&callback_uri_for_signals)
                .json(&response_to_send)
                .send()
                .await
                .and_then(|r| r.error_for_status());

            match send_result {
                Ok(_) => info!("Successfully sent value for request {request:?}"),
                Err(e) => log::error!("Failed to send value to {request:?}: {e}"),
            }
        }
    });

    // Subscriber publish setup
    tokio::spawn(async move {
        let client = Client::new();
        loop {
            debug!("Beginning subscribe loop...");

            let subscriptions = {
                let state = subscribe_loop_state.lock().unwrap();
                state.subscriptions.clone()
            };

            for (entity_id, subscribers) in subscriptions {
                // Get provider value
                let value = {
                    let mut state = subscribe_loop_state.lock().unwrap();
                    get_entity_value(&mut state, &entity_id).unwrap_or(String::new())
                };

                if value.is_empty() && !subscribers.is_empty() {
                    warn!("Entity {entity_id} has subscriptions but wasn't found!");
                    continue;
                }

                for subscriber in subscribers {
                    let request = EntityValueResponse {
                        entity_id: entity_id.clone(),
                        value: value.clone(),
                    };

                    let send_result = client
                        .post(&subscriber)
                        .json(&request)
                        .send()
                        .await
                        .and_then(|r| r.error_for_status());

                    match send_result {
                        Ok(_) => debug!(
                            "Successfully sent value for request {request:?} to {subscriber}"
                        ),
                        Err(e) => error!(
                            "Failed to send value for request {request:?} to {subscriber}: {e}"
                        ),
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(3000)).await;
        }
    });

    // HTTP server setup
    info!(
        "Mock Digital Twin Adapter Server starting at {}",
        settings.digital_twin_server_authority
    );

    let app = Router::new()
        .route(ENTITY_PATH, get(get_entity))
        .route(ENTITY_SUBSCRIBE_PATH, post(subscribe))
        .route(ENTITY_GET_VALUE_PATH, post(request_value))
        .with_state(state);

    Server::bind(
        &settings
            .digital_twin_server_authority
            .parse::<SocketAddr>()
            .expect("unable to parse socket address"),
    )
    .serve(app.into_make_service())
    .await
    .unwrap();
}

/// Handles getting access info of an entity
///
/// # Arguments
/// - `state`: the state of the DigitalTwinAdapter which consists of active entities and their subscriptions
/// - `query`: the entity query you wish to get access info on
async fn get_entity(
    State(state): State<Arc<Mutex<DigitalTwinAdapterState>>>,
    extract::Query(query): extract::Query<EntityQuery>,
) -> Response {
    info!("Received request to get entity: {}", query.id);
    let state = state.lock().unwrap();
    find_entity(&state, &query.id)
        .map(|(config_item, _)| {
            let operation_path =
                if config_item.value.entity.operation.to_string() == SUBSCRIBE_OPERATION {
                    ENTITY_SUBSCRIBE_PATH
                } else if config_item.value.entity.operation.to_string() == GET_OPERATION {
                    ENTITY_GET_VALUE_PATH
                } else {
                    return server_error!("Entity didn't have a valid operation");
                };

            let entity = Entity {
                id: config_item.value.entity.id.clone(),
                name: config_item.value.entity.name.clone(),
                uri: format!("{}{operation_path}", config_item.value.entity.uri),
                description: config_item.value.entity.description.clone(),
                operation: config_item.value.entity.operation.clone(),
                protocol: config_item.value.entity.protocol.clone(),
            };

            ok!(GetDigitalTwinProviderResponse { entity })
        })
        .unwrap_or(not_found!())
}

/// Handles subscribe requests to an entity
///
/// # Arguments
/// - `state`: the state of the DigitalTwinAdapter which consists of active providers and their subscriptions
/// - `request`: the subscribe request to an entity
async fn subscribe(
    State(state): State<Arc<Mutex<DigitalTwinAdapterState>>>,
    Json(request): Json<EntityValueRequest>,
) -> Response {
    info!("Received subscribe request: {request:?}");
    let mut state = state.lock().unwrap();

    match find_entity(&state, &request.entity_id) {
        Some(_) => {
            state
                .subscriptions
                .entry(request.entity_id)
                .and_modify(|e| {
                    e.insert(request.callback_uri);
                });
            ok!()
        }
        None => not_found!(),
    }
}

/// Handles async get requests
///
/// # Arguments
/// - `state`: the state of the DigitalTwinAdapter which consists of active providers
/// - `request`: the async get request to an entity
async fn request_value(
    State(state): State<Arc<Mutex<DigitalTwinAdapterState>>>,
    Json(request): Json<EntityValueRequest>,
) -> Response {
    info!("Received request to get value: {request:?}");
    let mut state = state.lock().unwrap();
    match get_entity_value(&mut state, &request.entity_id) {
        Some(value) => {
            let response = EntityValueResponse {
                entity_id: request.entity_id,
                value,
            };

            info!("Submitting request...");
            match state
                .response_channel_sender
                .send((request.callback_uri, response))
            {
                Ok(_) => ok!(),
                Err(e) => server_error!(format!("Request value error: {e:?}")),
            }
        }
        None => not_found!(),
    }
}

/// Checks if a value is within bounds
///
/// # Arguments
/// - `value`: the value to check within bounds
/// - `begin`: the start of a boundary
/// - `end`: the end of a boundary
fn within_bounds(value: u8, begin: u8, end: Option<u8>) -> bool {
    match end {
        Some(end) => value >= begin && value < end,
        None => value >= begin,
    }
}

/// Gets active entity names for this mock provider
///
/// # Arguments
/// - `state`: the state of the DigitalTwinAdapter which consists of active entities
fn get_active_entity_names(state: &DigitalTwinAdapterState) -> Vec<String> {
    state
        .entities
        .iter()
        .filter_map(|(config_item, _)| {
            if within_bounds(state.count, config_item.begin, config_item.end) {
                Some(
                    config_item
                        .value
                        .entity
                        .name
                        .clone()
                        .unwrap_or_else(|| config_item.value.entity.id.clone()),
                )
            } else {
                None
            }
        })
        .collect()
}

/// Finds an entity using an entity's ID
///
/// # Arguments
/// - `state`: the state of the DigitalTwinAdapter which consists of active entities
/// - `id`: the entity's ID
fn find_entity<'a>(
    state: &'a DigitalTwinAdapterState,
    id: &'a String,
) -> Option<&'a (ConfigItem, u8)> {
    state
        .entities
        .iter()
        .filter(|(config_item, _)| within_bounds(state.count, config_item.begin, config_item.end))
        .find(|(config_item, _)| config_item.value.entity.id == *id)
}

/// Gets an entity's value
///
/// # Arguments
/// - `state`: the state of the DigitalTwinAdapter which consists of active entities
/// - `id`: the entity's ID
fn get_entity_value(state: &mut DigitalTwinAdapterState, id: &str) -> Option<String> {
    let n = state.count;
    state
        .entities
        .iter_mut()
        .filter(|(config_item, _)| within_bounds(n, config_item.begin, config_item.end))
        .find(|(config_item, _)| config_item.value.entity.id == *id)
        .map(|p| {
            p.1 += 1;
            p.0.value.values.get_nth(p.1 - 1)
        })
}