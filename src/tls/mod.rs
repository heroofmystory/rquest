//! TLS configuration
//!
//! By default, a `Client` will make use of BoringSSL for TLS.
//!
//! - Various parts of TLS can also be configured or even disabled on the
//!   `ClientBuilder`.

#![allow(missing_docs)]
mod conn;
mod extension;
mod impersonate;
mod settings;

use crate::{connect::HttpConnector, HttpVersionPref};
use boring::{
    error::ErrorStack,
    ssl::{SslConnector, SslMethod, SslOptions, SslVersion},
};
pub use conn::MaybeHttpsStream;
use conn::{HttpsConnector, HttpsLayer, HttpsLayerSettings};
pub use extension::{cert_compression, TlsConnectExtension, TlsExtension};
pub use impersonate::{chrome, firefox, okhttp, safari, tls_settings, Impersonate};
pub use settings::{Http2Settings, ImpersonateSettings, RootCertsStore, TlsSettings};

type TlsResult<T> = Result<T, ErrorStack>;

/// A wrapper around a `SslConnectorBuilder` that allows for additional settings.
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct BoringTlsConnector {
    tls_sni: bool,
    enable_ech_grease: bool,
    application_settings: bool,
    alpn_protos: HttpVersionPref,
    connect_layer: HttpsLayer,
}

impl BoringTlsConnector {
    /// Create a new `BoringTlsConnector` with the given function.
    #[inline]
    pub fn new(settings: TlsSettings) -> TlsResult<BoringTlsConnector> {
        Ok(Self {
            tls_sni: settings.tls_sni,
            enable_ech_grease: settings.enable_ech_grease,
            application_settings: settings.application_settings,
            alpn_protos: settings.alpn_protos,
            connect_layer: connect_layer(settings)?,
        })
    }

    /// Create a new `HttpsConnector` with the settings from the `HttpConnector`.
    #[inline]
    pub(crate) async fn create_connector(
        &self,
        http: HttpConnector,
        ws: bool,
    ) -> HttpsConnector<HttpConnector> {
        // Create the `HttpsConnector` with the given `HttpConnector` and `ConnectLayer`.
        let mut http = HttpsConnector::with_connector_layer(http, self.connect_layer.clone());

        // Set the callback to add application settings.
        let (application_settings, enable_ech_grease, alpn_protos, tls_sni) = (
            self.application_settings,
            self.enable_ech_grease,
            self.alpn_protos,
            self.tls_sni,
        );
        http.set_callback(move |conf, _| {
            conf.configure_enable_ech_grease(enable_ech_grease)?
                .set_verify_hostname(tls_sni);

            // Add application settings if it is set.
            if application_settings {
                conf.configure_add_application_settings(alpn_protos)?;
            }

            // Set websocket use http1 alpn proto
            if ws {
                conf.set_alpn_protos(b"\x08http/1.1")?;
            }

            Ok(())
        });

        http
    }
}

/// Create a new `ConnectLayer` with the given `Tls` settings.
#[inline]
fn connect_layer(settings: TlsSettings) -> TlsResult<HttpsLayer> {
    let default_connector = if cfg!(any(
        feature = "boring-tls-native-roots",
        feature = "boring-tls-webpki-roots"
    )) {
        SslConnector::no_default_verify_builder(SslMethod::tls_client())
    } else {
        SslConnector::builder(SslMethod::tls_client())
    };

    // If the connector builder is set, use it. Otherwise, create a new one.
    let connector = settings
        .connector
        .map(Result::Ok)
        .unwrap_or(default_connector)?;

    // Create the `SslConnectorBuilder` and configure it.
    let mut connector = connector
        .configure_cert_verification(settings.certs_verification)?
        .configure_alpn_protos(settings.alpn_protos)?
        .configure_min_tls_version(settings.min_tls_version)?
        .configure_max_tls_version(settings.max_tls_version)?;

    // Set enable ocsp stapling if it is set.
    if settings.enable_ocsp_stapling {
        connector.enable_ocsp_stapling();
    }

    // Set enable signed cert timestamps if it is set.
    if settings.enable_signed_cert_timestamps {
        connector.enable_signed_cert_timestamps();
    }

    // Set no session ticket if it is set.
    if let Some(false) = settings.session_ticket {
        connector.set_options(SslOptions::NO_TICKET);
    }

    // Set grease enabled if it is set.
    if let Some(grease_enabled) = settings.grease_enabled {
        connector.set_grease_enabled(grease_enabled);
    }

    // Set permute extensions if it is set.
    if let Some(permute_extensions) = settings.permute_extensions {
        connector.set_permute_extensions(permute_extensions);
    }

    // Set the curves if they are set.
    if let Some(curves) = settings.curves.as_deref() {
        connector.set_curves(curves)?;
    }

    // Set the signature algorithms list if it is set.
    if let Some(sigalgs_list) = settings.sigalgs_list.as_deref() {
        connector.set_sigalgs_list(sigalgs_list)?;
    }

    // Set the delegated credentials if it is set.
    if let Some(delegated_credentials) = settings.delegated_credentials.as_deref() {
        connector.set_delegated_credentials(delegated_credentials)?;
    }

    // Set the cipher list if it is set.
    if let Some(cipher_list) = settings.cipher_list.as_deref() {
        connector.set_cipher_list(cipher_list)?;
    }

    // Set the certificate compression algorithm if it is set.
    if let Some(cert_compression_algorithm) = settings.cert_compression_algorithm {
        for algorithm in cert_compression_algorithm.iter() {
            connector = connector.configure_add_cert_compression_alg(*algorithm)?;
        }
    }

    // Set the record size limit if it is set.
    if let Some(record_size_limit) = settings.record_size_limit {
        connector.set_record_size_limit(record_size_limit);
    }

    // Set the key shares length limit if it is set.
    if let Some(limit) = settings.key_shares_length_limit {
        connector.set_key_shares_length_limit(limit);
    }

    // Set the extension permutation if it is set.
    if let Some(extensions) = settings.extension_permutation {
        connector.set_extension_permutation(extensions.as_ref())?;
    }

    // Set the extension permutation index if it is set.
    if let Some(indices) = settings.extension_permutation_indices {
        connector.set_extension_permutation_indices(indices.as_ref())?;
    }

    // Conditionally configure the TLS builder based on the "boring-tls-native-roots" feature.
    // If no custom CA cert store, use the system's native certificate store if the feature is enabled.
    let connector = if settings.root_certs_store.is_none() {
        // WebPKI root certificates are enabled (regardless of whether native-roots is also enabled).
        #[cfg(any(
            feature = "boring-tls-webpki-roots",
            feature = "boring-tls-native-roots"
        ))]
        {
            connector.configure_set_verify_cert_store()?
        }

        // Neither native-roots nor WebPKI roots are enabled, proceed with the default builder.
        #[cfg(not(any(
            feature = "boring-tls-native-roots",
            feature = "boring-tls-webpki-roots"
        )))]
        {
            connector
        }
    } else {
        // If a custom CA certificate store is provided, configure it.
        connector.configure_ca_cert_store(settings.root_certs_store)?
    };

    // Create the `HttpsLayerSettings` with the default session cache capacity.
    let settings = HttpsLayerSettings::builder()
        .session_cache_capacity(8)
        .session_cache(settings.pre_shared_key)
        .skip_session_ticket(settings.psk_skip_session_ticket)
        .build();

    HttpsLayer::with_connector_and_settings(connector, settings)
}

/// A TLS protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlsVersion(SslVersion);

// These could perhaps be From/TryFrom implementations, but those would be
// part of the public API so let's be careful
impl TlsVersion {
    /// Version 1.0 of the TLS protocol.
    pub const TLS_1_0: TlsVersion = TlsVersion(SslVersion::TLS1);
    /// Version 1.1 of the TLS protocol.
    pub const TLS_1_1: TlsVersion = TlsVersion(SslVersion::TLS1_1);
    /// Version 1.2 of the TLS protocol.
    pub const TLS_1_2: TlsVersion = TlsVersion(SslVersion::TLS1_2);
    /// Version 1.3 of the TLS protocol.
    pub const TLS_1_3: TlsVersion = TlsVersion(SslVersion::TLS1_3);
}

/// Hyper extension carrying extra TLS layer information.
/// Made available to clients on responses when `tls_info` is set.
#[derive(Debug, Clone)]
pub struct TlsInfo {
    pub(crate) peer_certificate: Option<Vec<u8>>,
}

impl TlsInfo {
    /// Get the DER encoded leaf certificate of the peer.
    pub fn peer_certificate(&self) -> Option<&[u8]> {
        self.peer_certificate.as_ref().map(|der| &der[..])
    }
}
