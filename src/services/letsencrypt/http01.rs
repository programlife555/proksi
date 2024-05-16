use std::{
    collections::HashMap,
    fs::{create_dir_all, File},
    io::{Read, Write},
    thread::sleep,
    time::Duration,
};

use async_trait::async_trait;
use instant_acme::{AccountCredentials, ChallengeType, KeyAuthorization, LetsEncrypt, Order};
use pingora::{server::ShutdownWatch, services::background::BackgroundService};
use rcgen::KeyPair;
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug)]
struct Storage {
    orders: HashMap<String, (String, String, KeyAuthorization)>,
}

impl Storage {
    pub fn new() -> Self {
        Storage {
            orders: HashMap::new(),
        }
    }

    pub fn add_order(
        &mut self,
        identifier: String,
        token: String,
        url: String,
        key_auth: KeyAuthorization,
    ) {
        self.orders.insert(identifier, (token, url, key_auth));
    }

    pub fn get_orders(&self) -> &HashMap<String, (String, String, KeyAuthorization)> {
        &self.orders
    }

    pub fn _get_order(&self, order: &str) -> Option<&(String, String, KeyAuthorization)> {
        self.orders.get(order)
    }
}

#[derive(Serialize, Deserialize)]
struct HostCertificate {
    host: String,
    cert: String,
    key: String,
    expires_at: i64,
}

#[derive(Serialize, Deserialize)]
struct HostChallenge {
    host: String,
    url: String,
    token: String,
    key_auth: String,
}

#[derive(Serialize, Deserialize)]
struct HostOrder {
    host: String,
    url: String,
}

#[derive(Debug)]
pub struct HttpLetsencryptService {
    challenge_type: ChallengeType,
    url: String,
    contact: String,
    hosts: Vec<String>,
    cert_store: Storage,
}

impl HttpLetsencryptService {
    pub fn new(hosts: &[String], contact: &str) -> Self {
        HttpLetsencryptService {
            challenge_type: ChallengeType::Http01,
            url: LetsEncrypt::Staging.url().to_string(),
            contact: contact.to_string(),
            hosts: hosts.to_vec(),
            cert_store: Storage::new(),
        }
    }

    async fn create_account(
        &self,
    ) -> Result<(instant_acme::Account, instant_acme::AccountCredentials), ()> {
        // Fetch an existing account
        let mut existing_credentials: String = String::new();
        let file = File::open(format!("{}/credentials.json", self.account_path()));

        if let Ok(mut file) = file {
            file.read_to_string(&mut existing_credentials).unwrap();

            let credentials =
                serde_json::from_str::<AccountCredentials>(&existing_credentials).unwrap();

            let long_lived =
                serde_json::from_str::<AccountCredentials>(&existing_credentials).unwrap();

            let account = instant_acme::Account::from_credentials(credentials)
                .await
                .map_err(|acc| format!("Failed to fetch account {}", acc))
                .unwrap();

            println!("Fetched existing account");
            return Ok((account, long_lived));
        }

        // Else create a new account
        let new_account = instant_acme::NewAccount {
            contact: &[&format!("mailto:{}", self.contact)],
            terms_of_service_agreed: true,
            only_return_existing: false,
        };

        let account = instant_acme::Account::create(&new_account, &self.url, None)
            .await
            .map_err(|acc| format!("Failed to create account {}", acc));

        match account {
            Ok((account, credentials)) => {
                // write credentials to file
                let file =
                    File::create(format!("{}/credentials.json", self.account_path())).unwrap();
                serde_json::to_writer(file, &credentials).unwrap();
                Ok((account, credentials))
            }
            Err(e) => {
                println!("Failed to created account: {:?}", e);
                Err(())
            }
        }
    }

    /// Create a new order
    async fn create_order(&self, excluded_hosts: Vec<String>) -> Result<instant_acme::Order, ()> {
        let (account, _credentials) = self.create_account().await.unwrap();
        let mut identifiers = Vec::with_capacity(self.hosts.len());

        // Push all the hosts into the identifiers
        // TODO: create orders in groups of 20 hosts (for performance reasons)
        for host in self.hosts.iter() {
            if excluded_hosts.contains(host) {
                continue;
            }

            let identifier = instant_acme::Identifier::Dns(host.to_owned());
            identifiers.push(identifier);
        }

        // Nothing to do if there are no identifiers
        if identifiers.is_empty() {
            return Err(());
        }

        // Create a new order with the domain names
        let order = account
            .new_order(&instant_acme::NewOrder {
                identifiers: &identifiers,
            })
            .await
            .map_err(|order| format!("Failed to create order {}", order))
            .unwrap();

        Ok(order)
    }

    /// Create challenges for the order
    async fn create_challenges_from_order(
        &mut self,
        excluded_hosts: Vec<String>,
    ) -> Result<Order, ()> {
        println!("Creating challenges from order");
        let order = self.create_order(excluded_hosts).await;
        if order.is_err() {
            println!("Order error {:?}", order.err().unwrap());
            return Err(());
        }

        let mut order_result = order.unwrap();
        let authorizations = order_result.authorizations().await.unwrap();

        for authz in &authorizations {
            match authz.status {
                instant_acme::AuthorizationStatus::Pending => continue,
                instant_acme::AuthorizationStatus::Valid => {}
                _ => return Err(()),
            }

            println!("Creating challenges for {:?}", authz.identifier);

            let instant_acme::Identifier::Dns(identifier) = &authz.identifier;
            let challenge = authz
                .challenges
                .iter()
                .find(|cha| cha.r#type == self.challenge_type)
                .ok_or(format!("No {:?} challenge found", self.challenge_type))
                .unwrap();

            let key_auth = order_result.key_authorization(challenge);

            // let mut store = self.cert_store;

            self.cert_store.add_order(
                identifier.clone(),
                challenge.token.clone(),
                challenge.url.clone(),
                key_auth,
            );
        }

        Ok(order_result)
    }

    fn challenges_path(&self) -> &str {
        "./data/challenges"
    }

    fn certificates_path(&self) -> &str {
        "./data/certificates"
    }

    fn account_path(&self) -> &str {
        "./data/account"
    }

    fn orders_path(&self) -> &str {
        "./data/orders"
    }
}

#[derive(Debug)]
pub struct HttpLetsencrypt {
    contact: String,
    hosts: Vec<String>,
}

impl HttpLetsencrypt {
    pub fn new(hosts: &[String], contact: &str) -> Self {
        HttpLetsencrypt {
            contact: contact.to_string(),
            hosts: hosts.to_vec(),
        }
    }
}

#[async_trait]
impl BackgroundService for HttpLetsencrypt {
    async fn start(&self, _shutdown: ShutdownWatch) -> () {
        info!(service = "LetsEncrypt", "Background service started");

        let mut svc = HttpLetsencryptService::new(&self.hosts, &self.contact);

        // create required folders if they don't exist yet
        create_dir_all(svc.challenges_path()).unwrap();
        create_dir_all(svc.certificates_path()).unwrap();
        create_dir_all(svc.account_path()).unwrap();
        create_dir_all(svc.orders_path()).unwrap();

        // Check if we already have a challenge file
        let mut excluded_hosts = Vec::new();
        for host in svc.hosts.iter() {
            let file = std::fs::File::open(format!("./data/challenges/{}/meta.csv", host));

            if file.is_ok() {
                info!("Already found {} in the list of challenges", host);
                excluded_hosts.push(host.clone());
            }
        }

        if excluded_hosts.len() == svc.hosts.len() {
            info!("All hosts have a challenge file");
            return;
        }

        // Creates order if there are outstanding hosts to check
        let order = svc
            .create_challenges_from_order(excluded_hosts.clone())
            .await;

        if order.is_err() {
            info!("No order to check");
            return;
        }

        let mut order = order.unwrap();

        // 1. persist order to disk
        let mut file = File::create(format!("{}/meta.txt", svc.orders_path())).unwrap();
        let contents = format!("{:?}", order.url());
        file.write_all(contents.as_bytes()).unwrap();
        file.flush().unwrap();

        if svc.cert_store.get_orders().is_empty() {
            info!("No orders to check");
            return;
        }

        // write challenges to disk
        for (key, value) in svc.cert_store.get_orders().iter() {
            let (token, url, key_auth) = value;
            // Create a new folder for the challenge
            create_dir_all(format!("{}/{}", svc.challenges_path(), key)).unwrap();
            let mut file = File::create(format!("./data/challenges/{}/meta.csv", key)).unwrap();
            let contents = format!("{};{};{}", url, key_auth.as_str(), token);

            file.write_all(contents.as_bytes()).unwrap();
            file.flush().unwrap();

            println!("Setting challenge ready for {}", key);
            order.set_challenge_ready(url).await.unwrap();
        }

        let max_retries = 10;
        let mut current_retry = 0;
        let mut retry_delay = 1;

        while order.state().status != instant_acme::OrderStatus::Ready {
            if current_retry >= max_retries {
                info!("Max retries reached");
                return;
            }

            info!(
                "Waiting for order to be ready, attempt #{}...",
                current_retry
            );
            sleep(Duration::from_secs(retry_delay));
            order.refresh().await.unwrap();

            current_retry += 1;
            retry_delay *= 2;
        }

        let non_excluded_hosts = svc
            .hosts
            .iter()
            .filter(|&host| !excluded_hosts.contains(host))
            .cloned()
            .collect::<Vec<String>>();

        info!("Generating certificates for {:?}", non_excluded_hosts);

        let kp = KeyPair::generate().unwrap();

        let mut cert_params = rcgen::CertificateParams::new(non_excluded_hosts.clone()).unwrap();
        cert_params.distinguished_name = rcgen::DistinguishedName::new();
        let csr = cert_params.serialize_request(&kp).unwrap();

        let status = order.finalize(csr.der()).await;
        if status.is_err() {
            println!("Failed to finalize order: {:?}", status.err().unwrap());
            return;
        }

        // Order is ready, download the certificate
        let cert_chain = loop {
            match order.certificate().await {
                Ok(Some(cert)) => {
                    info!("Cert ready");
                    break cert;
                }
                Ok(None) => {
                    info!("Cert not ready yet, waiting 5 seconds...");
                    sleep(Duration::from_secs(5));
                }
                Err(e) => {
                    info!("Error downloading cert: {:?}", e);
                    return;
                }
            }
        };

        // for each host, write the certificate to disk

        for host in non_excluded_hosts.iter() {
            //create folder for the certificate
            create_dir_all(format!("{}/{}", svc.certificates_path(), host)).unwrap();
            let mut crt_file =
                File::create(format!("{}/{}/cert.pem", svc.certificates_path(), host)).unwrap();
            let mut key_file =
                File::create(format!("{}/{}/key.pem", svc.certificates_path(), host)).unwrap();

            crt_file.write_all(cert_chain.as_bytes()).unwrap();
            key_file.write_all(kp.serialize_pem().as_bytes()).unwrap();
            crt_file.flush().unwrap();
            key_file.flush().unwrap();

            info!("Certificate written to disk for {}", host);
        }

        // write certificate to disk
        // let mut file = File::create(format!("{}/cert.pem", self.certificates_path())).unwrap();

        return;
        // loop over the order state until all challenges are valid
        // loop {
        //     let order = authz.unwrap();
        //     let status = order..await.unwrap();

        //     if status == instant_acme::OrderStatus::Ready {
        //         break;
        //     }

        //     if status == instant_acme::OrderStatus::Invalid {
        //         println!("Order is invalid");
        //         break;
        //     }

        //     println!("Order status: {:?}", status);
        //     tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        // }
    }
}
