//
// Copyright 2024 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//
use std::net::Ipv4Addr;
use std::num::NonZeroU16;

use attest::svr2::RaftConfig;
use const_str::ip_addr;
use libsignal_net::chat::RECOMMENDED_CHAT_WS_CONFIG;
use libsignal_net::connect_state::ServiceName;
use libsignal_net::enclave::{Cdsi, EnclaveEndpoint, EndpointParams, MrEnclave, SvrSgx};
use libsignal_net::env::{ConnectionConfig, DomainConfig, Env, KeyTransConfig, Svr2Env, SvrBEnv};
use libsignal_net::infra::RECOMMENDED_WS_CONFIG;
use libsignal_net::infra::certs::RootCertificates;
use libsignal_net::infra::route::HttpVersion;

const ENCLAVE_ID_MOCK_SERVER: &[u8] = b"0.20240911.184407";

fn localhost_test_domain_config_with_port_and_cert(
    service: ServiceName,
    port: NonZeroU16,
    root_certificate_der: &[u8],
    http_version: HttpVersion,
) -> DomainConfig {
    const LOCALHOST_IP_V4: Ipv4Addr = ip_addr!(v4, "127.0.0.1");
    DomainConfig {
        ip_v4: &[LOCALHOST_IP_V4],
        ip_v6: &[],
        connect: ConnectionConfig {
            service,
            hostname: "localhost",
            port,
            cert: RootCertificates::FromDer(std::borrow::Cow::Owned(root_certificate_der.to_vec())),
            http_version: Some(http_version),
            min_tls_version: None,
            confirmation_header_name: None,
            proxy: None,
        },
    }
}

pub(crate) struct LocalhostEnvPortConfig {
    pub(crate) chat_port: NonZeroU16,
    pub(crate) cdsi_port: NonZeroU16,
    pub(crate) svr2_port: NonZeroU16,
    pub(crate) svrb_port: NonZeroU16,
}

const DUMMY_RAFT_CONFIG: &RaftConfig = &RaftConfig {
    min_voting_replicas: 3,
    max_voting_replicas: 9,
    super_majority: 0,
    group_id: 17325409821474389983,
    attestation_timeout: 604800,
    db_version: 2,
    simulated: false,
};

const DUMMY_CDSI_ENDPOINT_PARAMS: EndpointParams<'static, Cdsi> = EndpointParams {
    mr_enclave: MrEnclave::new(ENCLAVE_ID_MOCK_SERVER),
    raft_config: (),
};

const DUMMY_SVR2_ENDPOINT_PARAMS: EndpointParams<'static, SvrSgx> = EndpointParams {
    mr_enclave: MrEnclave::new(ENCLAVE_ID_MOCK_SERVER),
    raft_config: DUMMY_RAFT_CONFIG,
};

const DUMMY_SVRB_ENDPOINT_PARAMS: EndpointParams<'static, SvrSgx> = EndpointParams {
    mr_enclave: MrEnclave::new(ENCLAVE_ID_MOCK_SERVER),
    raft_config: DUMMY_RAFT_CONFIG,
};

const DUMMY_KEYTRANS_CONFIG: KeyTransConfig = KeyTransConfig {
    signing_key_material: &[0; 32],
    vrf_key_material: &[0; 32],
    auditor_key_material: &[&[0; 32]],
};

pub(crate) fn localhost_test_env_with_ports(
    ports: LocalhostEnvPortConfig,
    root_certificate_der: &[u8],
    http_version: HttpVersion,
) -> Env<'static> {
    Env {
        chat_domain_config: localhost_test_domain_config_with_port_and_cert(
            ServiceName("chat"),
            ports.chat_port,
            root_certificate_der,
            http_version,
        ),
        chat_ws_config: RECOMMENDED_CHAT_WS_CONFIG,
        cdsi: EnclaveEndpoint {
            domain_config: localhost_test_domain_config_with_port_and_cert(
                ServiceName("cdsi"),
                ports.cdsi_port,
                root_certificate_der,
                http_version,
            ),
            ws_config: RECOMMENDED_WS_CONFIG,
            params: DUMMY_CDSI_ENDPOINT_PARAMS,
        },
        svr2: Svr2Env {
            current: EnclaveEndpoint {
                domain_config: localhost_test_domain_config_with_port_and_cert(
                    ServiceName("svr2"),
                    ports.svr2_port,
                    root_certificate_der,
                    http_version,
                ),
                ws_config: RECOMMENDED_WS_CONFIG,
                params: DUMMY_SVR2_ENDPOINT_PARAMS,
            },
            previous: None,
        },
        svr_b: SvrBEnv::new(
            [
                Some(EnclaveEndpoint {
                    domain_config: localhost_test_domain_config_with_port_and_cert(
                        ServiceName("svrb"),
                        ports.svrb_port,
                        root_certificate_der,
                        http_version,
                    ),
                    ws_config: RECOMMENDED_WS_CONFIG,
                    params: DUMMY_SVRB_ENDPOINT_PARAMS,
                }),
                None,
                None,
            ],
            [None, None, None],
        ),
        keytrans_config: DUMMY_KEYTRANS_CONFIG,
        reflector_providers: || &[],
    }
}

/// Tellomi: a self-hosted Signal-Server reachable at an arbitrary hostname (e.g. `chat.tellomi.app`).
///
/// Like [`localhost_test_env_with_ports`] but the hostname is real (DNS-resolved, no static IPs),
/// TLS 1.3 is required and, when `root_certificate_der` is empty, the platform trust store is used
/// (so a Let's Encrypt certificate works out of the box).
fn custom_server_domain_config(
    service: ServiceName,
    hostname: &'static str,
    port: NonZeroU16,
    root_certificate_der: &[u8],
    http_version: HttpVersion,
) -> DomainConfig {
    DomainConfig {
        ip_v4: &[],
        ip_v6: &[],
        connect: ConnectionConfig {
            service,
            hostname,
            port,
            cert: if root_certificate_der.is_empty() {
                RootCertificates::Native
            } else {
                RootCertificates::FromDer(std::borrow::Cow::Owned(root_certificate_der.to_vec()))
            },
            http_version: Some(http_version),
            min_tls_version: Some(boring_signal::ssl::SslVersion::TLS1_3),
            confirmation_header_name: None,
            proxy: None,
        },
    }
}

pub(crate) fn custom_server_env(
    hostname: &str,
    ports: LocalhostEnvPortConfig,
    root_certificate_der: &[u8],
    http_version: HttpVersion,
) -> Env<'static> {
    // `Env<'static>` wants `&'static str`; one leak per Net construction is acceptable.
    let hostname: &'static str = Box::leak(hostname.to_owned().into_boxed_str());
    let dc = |service, port| custom_server_domain_config(service, hostname, port, root_certificate_der, http_version);
    Env {
        chat_domain_config: dc(ServiceName("chat"), ports.chat_port),
        chat_ws_config: RECOMMENDED_CHAT_WS_CONFIG,
        cdsi: EnclaveEndpoint {
            domain_config: dc(ServiceName("cdsi"), ports.cdsi_port),
            ws_config: RECOMMENDED_WS_CONFIG,
            params: DUMMY_CDSI_ENDPOINT_PARAMS,
        },
        svr2: Svr2Env {
            current: EnclaveEndpoint {
                domain_config: dc(ServiceName("svr2"), ports.svr2_port),
                ws_config: RECOMMENDED_WS_CONFIG,
                params: DUMMY_SVR2_ENDPOINT_PARAMS,
            },
            previous: None,
        },
        svr_b: SvrBEnv::new(
            [
                Some(EnclaveEndpoint {
                    domain_config: dc(ServiceName("svrb"), ports.svrb_port),
                    ws_config: RECOMMENDED_WS_CONFIG,
                    params: DUMMY_SVRB_ENDPOINT_PARAMS,
                }),
                None,
                None,
            ],
            [None, None, None],
        ),
        keytrans_config: DUMMY_KEYTRANS_CONFIG,
        reflector_providers: || &[],
    }
}
