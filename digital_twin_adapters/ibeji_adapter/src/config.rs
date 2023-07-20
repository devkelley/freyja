// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};

pub(crate) const CONFIG_FILE: &str = "config.json";

/// Configuration setting variants for selecting the service
/// that the Ibeji Adapter should communicate with to interact with Ibeji
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "service_type")]
pub enum Settings {
    /// In-Vehicle Digital Twin Service
    InVehicleDigitalTwinService { uri: String },

    /// Chariott's Service Discovery to discover Ibeji
    ChariottDiscoveryService {
        uri: String,
        metadata: IbejiDiscoveryMetadata,
    },
}

/// Configuration metadata for discovering Ibeji using Chariott
#[derive(Clone, Serialize, Deserialize)]
pub struct IbejiDiscoveryMetadata {
    pub namespace: String,
    pub name: String,
    pub version: String,
}