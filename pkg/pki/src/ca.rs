use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
use tracing::info;

/// Internal cluster Certificate Authority.
/// Generates a self-signed root CA and issues per-node TLS certificates.
pub struct ClusterCA {
    ca_cert_pem: String,
    ca_key_pair: KeyPair,
    ca_cert: rcgen::Certificate,
}

impl ClusterCA {
    /// Create a new CA with a freshly-generated self-signed root certificate.
    pub fn new() -> anyhow::Result<Self> {
        info!("Generating internal Cluster CA");

        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "k3rs Cluster CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "k3rs");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

        let key_pair = KeyPair::generate()?;
        let ca_cert = params.self_signed(&key_pair)?;
        let ca_cert_pem = ca_cert.pem();

        info!("Cluster CA generated successfully");

        Ok(Self {
            ca_cert_pem,
            ca_key_pair: key_pair,
            ca_cert,
        })
    }

    /// Issue a TLS certificate for a node, signed by this CA.
    /// Returns `(cert_pem, private_key_pem)`.
    pub fn issue_node_cert(&self, node_name: &str) -> anyhow::Result<(String, String)> {
        info!("Issuing certificate for node: {}", node_name);

        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, node_name);
        params
            .distinguished_name
            .push(DnType::OrganizationName, "k3rs-nodes");
        params.is_ca = IsCa::NoCa;
        params
            .subject_alt_names
            .push(rcgen::SanType::DnsName(node_name.try_into()?));

        let node_key = KeyPair::generate()?;
        let node_cert = params.signed_by(&node_key, &self.ca_cert, &self.ca_key_pair)?;

        Ok((node_cert.pem(), node_key.serialize_pem()))
    }

    /// Return the CA certificate PEM so agents can verify server identity.
    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert_pem
    }
}
