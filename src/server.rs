// Copyright 2024 Cloudflavor GmbH

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at

// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use tonic::async_trait;

use crate::nubium_opal_v1;
use crate::nubium_opal_v1::opal_server::Opal;

#[derive(Debug, Clone)]
pub struct OpalControlPlane;

#[async_trait]
impl Opal for OpalControlPlane {
    async fn scale_instance(
        &self,
        _request: tonic::Request<nubium_opal_v1::ScaleInstanceRequest>,
    ) -> Result<tonic::Response<nubium_opal_v1::ScaleInstanceResponse>, tonic::Status> {
        unimplemented!()
    }

    async fn push_ingress_config(
        &self,
        _request: tonic::Request<nubium_opal_v1::PushIngressConfigRequest>,
    ) -> Result<tonic::Response<nubium_opal_v1::PushIngressConfigResponse>, tonic::Status> {
        unimplemented!()
    }

    async fn push_load_balancer_config(
        &self,
        _request: tonic::Request<nubium_opal_v1::PushLoadBalancerConfigRequest>,
    ) -> Result<tonic::Response<nubium_opal_v1::PushLoadBalancerConfigResponse>, tonic::Status>
    {
        unimplemented!()
    }
}
