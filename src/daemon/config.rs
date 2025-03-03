use std::{
    env, fmt,
    fs::File,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::Duration;
use clap::{App, Arg};
use log::{error, LevelFilter};
use serde::{de, Deserialize, Deserializer};

#[cfg(unix)]
use syslog::Facility;

use rpki::{repository::x509::Time, uri};

use crate::{
    commons::{
        api::{PublicationServerUris, PublisherHandle, Token},
        error::KrillIoError,
        util::ext_serde,
    },
    constants::*,
    daemon::http::tls_keys,
};

#[cfg(feature = "multi-user")]
use crate::daemon::auth::providers::{config_file::config::ConfigAuthUsers, openid_connect::ConfigAuthOpenIDConnect};

//------------ ConfigDefaults ------------------------------------------------

pub struct ConfigDefaults;

impl ConfigDefaults {
    fn ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
    }
    fn port() -> u16 {
        3000
    }

    fn https_mode() -> HttpsMode {
        HttpsMode::Generate
    }
    fn data_dir() -> PathBuf {
        PathBuf::from("./data")
    }

    fn always_recover_data() -> bool {
        env::var(KRILL_ENV_FORCE_RECOVER).is_ok()
    }

    fn log_level() -> LevelFilter {
        match env::var(KRILL_ENV_LOG_LEVEL) {
            Ok(level) => match LevelFilter::from_str(&level) {
                Ok(level) => level,
                Err(_) => {
                    eprintln!("Unrecognized value for log level in env var {}", KRILL_ENV_LOG_LEVEL);
                    ::std::process::exit(1);
                }
            },
            _ => LevelFilter::Info,
        }
    }

    fn log_type() -> LogType {
        LogType::File
    }

    fn log_file() -> PathBuf {
        PathBuf::from("./krill.log")
    }

    fn syslog_facility() -> String {
        "daemon".to_string()
    }

    fn auth_type() -> AuthType {
        AuthType::AdminToken
    }

    fn admin_token() -> Token {
        match env::var(KRILL_ENV_ADMIN_TOKEN) {
            Ok(token) => Token::from(token),
            Err(_) => match env::var(KRILL_ENV_ADMIN_TOKEN_DEPRECATED) {
                Ok(token) => Token::from(token),
                Err(_) => {
                    eprintln!("You MUST provide a value for the \"admin token\", either by setting \"admin_token\" in the config file, or by setting the KRILL_ADMIN_TOKEN environment variable.");
                    ::std::process::exit(1);
                }
            },
        }
    }

    #[cfg(feature = "multi-user")]
    fn auth_policies() -> Vec<PathBuf> {
        vec![]
    }

    #[cfg(feature = "multi-user")]
    fn auth_private_attributes() -> Vec<String> {
        vec![]
    }

    fn ca_refresh_seconds() -> u32 {
        600
    }

    fn ca_refresh_parents_batch_size() -> usize {
        25
    }

    fn post_limit_api() -> u64 {
        256 * 1024 // 256kB
    }

    fn post_limit_rfc8181() -> u64 {
        32 * 1024 * 1024 // 32MB (roughly 8000 issued certificates, so a key roll for nicbr and 100% uptake should be okay)
    }

    fn rfc8181_log_dir() -> Option<PathBuf> {
        None
    }

    fn post_limit_rfc6492() -> u64 {
        1024 * 1024 // 1MB (for ref. the NIC br cert is about 200kB)
    }

    fn rfc6492_log_dir() -> Option<PathBuf> {
        None
    }

    fn bgp_risdumps_enabled() -> bool {
        true
    }

    fn bgp_risdumps_v4_uri() -> String {
        "http://www.ris.ripe.net/dumps/riswhoisdump.IPv4.gz".to_string()
    }

    fn bgp_risdumps_v6_uri() -> String {
        "http://www.ris.ripe.net/dumps/riswhoisdump.IPv6.gz".to_string()
    }

    fn roa_aggregate_threshold() -> usize {
        if let Ok(from_env) = env::var("KRILL_ROA_AGGREGATE_THRESHOLD") {
            if let Ok(nr) = usize::from_str(&from_env) {
                return nr;
            }
        }
        100
    }

    fn roa_deaggregate_threshold() -> usize {
        if let Ok(from_env) = env::var("KRILL_ROA_DEAGGREGATE_THRESHOLD") {
            if let Ok(nr) = usize::from_str(&from_env) {
                return nr;
            }
        }
        90
    }

    fn timing_publish_next_hours() -> i64 {
        24
    }

    fn timing_publish_next_jitter_hours() -> i64 {
        4
    }

    fn timing_publish_hours_before_next() -> i64 {
        8
    }

    fn timing_child_certificate_valid_weeks() -> i64 {
        52
    }

    fn timing_child_certificate_reissue_weeks_before() -> i64 {
        4
    }

    fn timing_roa_valid_weeks() -> i64 {
        52
    }

    fn timing_roa_reissue_weeks_before() -> i64 {
        4
    }

    fn timing_aspa_valid_weeks() -> i64 {
        52
    }

    fn timing_aspa_reissue_weeks_before() -> i64 {
        4
    }
}

//------------ Config --------------------------------------------------------

/// Global configuration for the Krill Server.
///
/// This will parse a default config file ('./defaults/krill.conf') unless
/// another file is explicitly specified. Command line arguments may be used
/// to override any of the settings in the config file.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(default = "ConfigDefaults::ip")]
    ip: IpAddr,

    #[serde(default = "ConfigDefaults::port")]
    pub port: u16,

    #[serde(default = "ConfigDefaults::https_mode")]
    https_mode: HttpsMode,

    #[serde(default = "ConfigDefaults::data_dir")]
    pub data_dir: PathBuf,

    #[serde(default = "ConfigDefaults::always_recover_data")]
    pub always_recover_data: bool,

    pub pid_file: Option<PathBuf>,

    service_uri: Option<uri::Https>,

    #[serde(
        default = "ConfigDefaults::log_level",
        deserialize_with = "ext_serde::de_level_filter"
    )]
    log_level: LevelFilter,

    #[serde(default = "ConfigDefaults::log_type")]
    log_type: LogType,

    #[serde(default = "ConfigDefaults::log_file")]
    log_file: PathBuf,

    #[serde(default = "ConfigDefaults::syslog_facility")]
    syslog_facility: String,

    #[serde(default = "ConfigDefaults::admin_token", alias = "auth_token")]
    pub admin_token: Token,

    #[serde(default = "ConfigDefaults::auth_type")]
    pub auth_type: AuthType,

    #[cfg(feature = "multi-user")]
    #[serde(default = "ConfigDefaults::auth_policies")]
    pub auth_policies: Vec<PathBuf>,

    #[cfg(feature = "multi-user")]
    #[serde(default = "ConfigDefaults::auth_private_attributes")]
    pub auth_private_attributes: Vec<String>,

    #[cfg(feature = "multi-user")]
    pub auth_users: Option<ConfigAuthUsers>,

    #[cfg(feature = "multi-user")]
    pub auth_openidconnect: Option<ConfigAuthOpenIDConnect>,

    #[serde(default = "ConfigDefaults::ca_refresh_seconds", alias = "ca_refresh")]
    pub ca_refresh_seconds: u32,

    #[serde(default = "ConfigDefaults::ca_refresh_parents_batch_size")]
    pub ca_refresh_parents_batch_size: usize,

    #[serde(skip)]
    suspend_child_after_inactive_seconds: Option<i64>,
    suspend_child_after_inactive_hours: Option<i64>,

    #[serde(default = "ConfigDefaults::post_limit_api")]
    pub post_limit_api: u64,

    #[serde(default = "ConfigDefaults::post_limit_rfc8181")]
    pub post_limit_rfc8181: u64,

    #[serde(default = "ConfigDefaults::rfc8181_log_dir")]
    pub rfc8181_log_dir: Option<PathBuf>,

    #[serde(default = "ConfigDefaults::post_limit_rfc6492")]
    pub post_limit_rfc6492: u64,

    #[serde(default = "ConfigDefaults::rfc6492_log_dir")]
    pub rfc6492_log_dir: Option<PathBuf>,

    // RIS BGP
    #[serde(default = "ConfigDefaults::bgp_risdumps_enabled")]
    pub bgp_risdumps_enabled: bool,
    #[serde(default = "ConfigDefaults::bgp_risdumps_v4_uri")]
    pub bgp_risdumps_v4_uri: String,
    #[serde(default = "ConfigDefaults::bgp_risdumps_v6_uri")]
    pub bgp_risdumps_v6_uri: String,

    // ROA Aggregation per ASN
    #[serde(default = "ConfigDefaults::roa_aggregate_threshold")]
    pub roa_aggregate_threshold: usize,

    #[serde(default = "ConfigDefaults::roa_deaggregate_threshold")]
    pub roa_deaggregate_threshold: usize,

    #[serde(flatten)]
    pub issuance_timing: IssuanceTimingConfig,

    #[serde(flatten)]
    pub repository_retention: RepositoryRetentionConfig,

    #[serde(flatten)]
    pub metrics: MetricsConfig,

    pub testbed: Option<TestBed>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IssuanceTimingConfig {
    #[serde(default = "ConfigDefaults::timing_publish_next_hours")]
    timing_publish_next_hours: i64,
    #[serde(default = "ConfigDefaults::timing_publish_next_jitter_hours")]
    timing_publish_next_jitter_hours: i64,
    #[serde(default = "ConfigDefaults::timing_publish_hours_before_next")]
    pub timing_publish_hours_before_next: i64,
    #[serde(default = "ConfigDefaults::timing_child_certificate_valid_weeks")]
    pub timing_child_certificate_valid_weeks: i64,
    #[serde(default = "ConfigDefaults::timing_child_certificate_reissue_weeks_before")]
    pub timing_child_certificate_reissue_weeks_before: i64,
    #[serde(default = "ConfigDefaults::timing_roa_valid_weeks")]
    pub timing_roa_valid_weeks: i64,
    #[serde(default = "ConfigDefaults::timing_roa_reissue_weeks_before")]
    pub timing_roa_reissue_weeks_before: i64,
    #[serde(default = "ConfigDefaults::timing_aspa_valid_weeks")]
    pub timing_aspa_valid_weeks: i64,
    #[serde(default = "ConfigDefaults::timing_aspa_reissue_weeks_before")]
    pub timing_aspa_reissue_weeks_before: i64,
}

impl IssuanceTimingConfig {
    /// Returns the next update time based on configuration:
    ///
    /// now + timing_publish_next_hours + random(0..timing_publish_next_jitter_hours)
    /// defaults: now + 24 hours + 0 to 4 hours
    pub fn publish_next(&self) -> Time {
        let regular_mins = self.timing_publish_next_hours * 60;
        let random_mins = {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            rng.gen_range(0..(60 * self.timing_publish_next_jitter_hours))
        };
        Time::now() + Duration::minutes(regular_mins + random_mins)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RepositoryRetentionConfig {
    #[serde(default = "RepositoryRetentionConfig::dflt_retention_old_notification_files_seconds")]
    pub retention_old_notification_files_seconds: i64,
    #[serde(default = "RepositoryRetentionConfig::dflt_retention_delta_files_min_nr")]
    pub retention_delta_files_min_nr: usize,
    #[serde(default = "RepositoryRetentionConfig::dflt_retention_delta_files_min_seconds")]
    pub retention_delta_files_min_seconds: i64,
    #[serde(default = "RepositoryRetentionConfig::dflt_retention_delta_files_max_nr")]
    pub retention_delta_files_max_nr: usize,
    #[serde(default = "RepositoryRetentionConfig::dflt_retention_delta_files_max_seconds")]
    pub retention_delta_files_max_seconds: i64,
    #[serde(default = "RepositoryRetentionConfig::dflt_retention_archive")]
    pub retention_archive: bool,
}

impl RepositoryRetentionConfig {
    // Time to keep any files still referenced by notification
    // files updated up to X seconds ago. We should not delete these
    // files too eagerly or we would risk that RPs with an old
    // notification file try to retrieve them, without success.
    //
    // Default: 10 min (just to be safe, 1 min is prob. fine)
    fn dflt_retention_old_notification_files_seconds() -> i64 {
        600
    }

    // Keep at least X (default 5) delta files in the notification
    // file, even if they would be too old. Their impact on the notification
    // file size is not too bad.
    fn dflt_retention_delta_files_min_nr() -> usize {
        5
    }

    // Minimum time to keep deltas. Defaults to 20 minutes, which
    // is double a commonly used update interval, allowing the vast
    // majority of RPs to update using deltas.
    fn dflt_retention_delta_files_min_seconds() -> i64 {
        1200 // 20 minutes
    }

    // Maximum time to keep deltas. Defaults to two hours meaning,
    // which is double to slowest normal update interval seen used
    // by a minority of RPs.
    fn dflt_retention_delta_files_max_seconds() -> i64 {
        7200 // 2 hours
    }

    // For files older than the min seconds specified (default 20 mins),
    // and younger than max seconds (2 hours), keep at most up to a total
    // nr of files X (default 50).
    fn dflt_retention_delta_files_max_nr() -> usize {
        50
    }

    // If set to true, we will archive - rather than delete - old
    // snapshot and delta files. The can then be backed up and/deleted
    // at the repository operator's discretion.
    fn dflt_retention_archive() -> bool {
        false
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct MetricsConfig {
    #[serde(default)] // false
    pub metrics_hide_ca_details: bool,
    #[serde(default)] // false
    pub metrics_hide_child_details: bool,
    #[serde(default)] // false
    pub metrics_hide_publisher_details: bool,
    #[serde(default)] // false
    pub metrics_hide_roa_details: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TestBed {
    ta_aia: uri::Rsync,
    ta_uri: uri::Https,
    rrdp_base_uri: uri::Https,
    rsync_jail: uri::Rsync,
}

impl TestBed {
    pub fn new(ta_aia: uri::Rsync, ta_uri: uri::Https, rrdp_base_uri: uri::Https, rsync_jail: uri::Rsync) -> Self {
        TestBed {
            ta_aia,
            ta_uri,
            rrdp_base_uri,
            rsync_jail,
        }
    }

    pub fn ta_aia(&self) -> &uri::Rsync {
        &self.ta_aia
    }

    pub fn ta_uri(&self) -> &uri::Https {
        &self.ta_uri
    }

    pub fn publication_server_uris(&self) -> PublicationServerUris {
        PublicationServerUris::new(self.rrdp_base_uri.clone(), self.rsync_jail.clone())
    }
}

/// # Accessors
impl Config {
    pub fn set_data_dir(&mut self, data_dir: PathBuf) {
        self.data_dir = data_dir;
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.ip, self.port)
    }

    pub fn test_ssl(&self) -> bool {
        self.https_mode == HttpsMode::Generate
    }

    pub fn https_cert_file(&self) -> PathBuf {
        let mut path = self.data_dir.clone();
        path.push(tls_keys::HTTPS_SUB_DIR);
        path.push(tls_keys::CERT_FILE);
        path
    }

    pub fn https_key_file(&self) -> PathBuf {
        let mut path = self.data_dir.clone();
        path.push(tls_keys::HTTPS_SUB_DIR);
        path.push(tls_keys::KEY_FILE);
        path
    }

    pub fn service_uri(&self) -> uri::Https {
        match &self.service_uri {
            None => {
                if self.ip == ConfigDefaults::ip() {
                    uri::Https::from_string(format!("https://localhost:{}/", self.port)).unwrap()
                } else {
                    uri::Https::from_string(format!("https://{}:{}/", self.ip, self.port)).unwrap()
                }
            }
            Some(uri) => uri.clone(),
        }
    }

    pub fn rfc8181_uri(&self, publisher: &PublisherHandle) -> uri::Https {
        uri::Https::from_string(format!("{}rfc8181/{}/", self.service_uri(), publisher)).unwrap()
    }

    pub fn pid_file(&self) -> PathBuf {
        match &self.pid_file {
            None => {
                let mut path = self.data_dir.clone();
                path.push("krill.pid");
                path
            }
            Some(file) => file.clone(),
        }
    }

    pub fn republish_hours(&self) -> i64 {
        if self.issuance_timing.timing_publish_hours_before_next < self.issuance_timing.timing_publish_next_hours {
            self.issuance_timing.timing_publish_next_hours - self.issuance_timing.timing_publish_hours_before_next
        } else {
            0
        }
    }

    pub fn suspend_child_after_inactive_seconds(&self) -> Option<i64> {
        match self.suspend_child_after_inactive_seconds {
            Some(seconds) => Some(seconds),
            None => self.suspend_child_after_inactive_hours.map(|hours| hours * 3600),
        }
    }

    pub fn testbed(&self) -> Option<&TestBed> {
        self.testbed.as_ref()
    }
}

/// # Create
impl Config {
    fn test_config(data_dir: &Path, enable_testbed: bool, enable_ca_refresh: bool, enable_suspend: bool) -> Self {
        use crate::test;

        let ip = ConfigDefaults::ip();
        let port = ConfigDefaults::port();
        let pid_file = None;

        let https_mode = HttpsMode::Generate;
        let data_dir = data_dir.to_path_buf();
        let always_recover_data = false;

        let log_level = LevelFilter::Debug;
        let log_type = LogType::Stderr;
        let mut log_file = data_dir.clone();
        log_file.push("krill.log");
        let syslog_facility = ConfigDefaults::syslog_facility();
        let auth_type = AuthType::AdminToken;
        let admin_token = Token::from("secret");
        #[cfg(feature = "multi-user")]
        let auth_policies = vec![];
        #[cfg(feature = "multi-user")]
        let auth_private_attributes = vec![];
        #[cfg(feature = "multi-user")]
        let auth_users = None;
        #[cfg(feature = "multi-user")]
        let auth_openidconnect = None;
        let ca_refresh_seconds = if enable_ca_refresh { 1 } else { 86400 };
        let ca_refresh_parents_batch_size = 10;
        let post_limit_api = ConfigDefaults::post_limit_api();
        let post_limit_rfc8181 = ConfigDefaults::post_limit_rfc8181();
        let rfc8181_log_dir = {
            let mut dir = data_dir.clone();
            dir.push("rfc8181");
            Some(dir)
        };
        let post_limit_rfc6492 = ConfigDefaults::post_limit_rfc6492();
        let rfc6492_log_dir = {
            let mut dir = data_dir.clone();
            dir.push("rfc6492");
            Some(dir)
        };

        let bgp_risdumps_enabled = false;
        let bgp_risdumps_v4_uri = ConfigDefaults::bgp_risdumps_v4_uri();
        let bgp_risdumps_v6_uri = ConfigDefaults::bgp_risdumps_v6_uri();

        let roa_aggregate_threshold = 3;
        let roa_deaggregate_threshold = 2;

        let timing_publish_next_hours = ConfigDefaults::timing_publish_next_hours();
        let timing_publish_next_jitter_hours = ConfigDefaults::timing_publish_next_jitter_hours();
        let timing_publish_hours_before_next = ConfigDefaults::timing_publish_hours_before_next();
        let timing_child_certificate_valid_weeks = ConfigDefaults::timing_child_certificate_valid_weeks();
        let timing_child_certificate_reissue_weeks_before =
            ConfigDefaults::timing_child_certificate_reissue_weeks_before();
        let timing_roa_valid_weeks = ConfigDefaults::timing_roa_valid_weeks();
        let timing_roa_reissue_weeks_before = ConfigDefaults::timing_roa_reissue_weeks_before();
        let timing_aspa_valid_weeks = ConfigDefaults::timing_aspa_valid_weeks();
        let timing_aspa_reissue_weeks_before = ConfigDefaults::timing_aspa_reissue_weeks_before();

        let issuance_timing = IssuanceTimingConfig {
            timing_publish_next_hours,
            timing_publish_next_jitter_hours,
            timing_publish_hours_before_next,
            timing_child_certificate_valid_weeks,
            timing_child_certificate_reissue_weeks_before,
            timing_roa_valid_weeks,
            timing_roa_reissue_weeks_before,
            timing_aspa_valid_weeks,
            timing_aspa_reissue_weeks_before,
        };

        let repository_retention = RepositoryRetentionConfig {
            retention_old_notification_files_seconds: 1,
            retention_delta_files_min_seconds: 0,
            retention_delta_files_min_nr: 5,
            retention_delta_files_max_seconds: 1,
            retention_delta_files_max_nr: 50,
            retention_archive: false,
        };

        let metrics = MetricsConfig {
            metrics_hide_ca_details: false,
            metrics_hide_child_details: false,
            metrics_hide_publisher_details: false,
            metrics_hide_roa_details: false,
        };

        let testbed = if enable_testbed {
            Some(TestBed::new(
                test::rsync("rsync://localhost/ta/ta.cer"),
                test::https("https://localhost/ta/ta.cer"),
                test::https("https://localhost/rrdp/"),
                test::rsync("rsync://localhost/repo/"),
            ))
        } else {
            None
        };

        let suspend_child_after_inactive_seconds = if enable_suspend { Some(3) } else { None };

        Config {
            ip,
            port,
            https_mode,
            data_dir,
            always_recover_data,
            pid_file,
            service_uri: None,
            log_level,
            log_type,
            log_file,
            syslog_facility,
            admin_token,
            auth_type,
            #[cfg(feature = "multi-user")]
            auth_policies,
            #[cfg(feature = "multi-user")]
            auth_private_attributes,
            #[cfg(feature = "multi-user")]
            auth_users,
            #[cfg(feature = "multi-user")]
            auth_openidconnect,
            ca_refresh_seconds,
            ca_refresh_parents_batch_size,
            suspend_child_after_inactive_seconds,
            suspend_child_after_inactive_hours: None,
            post_limit_api,
            post_limit_rfc8181,
            rfc8181_log_dir,
            post_limit_rfc6492,
            rfc6492_log_dir,
            bgp_risdumps_enabled,
            bgp_risdumps_v4_uri,
            bgp_risdumps_v6_uri,
            roa_aggregate_threshold,
            roa_deaggregate_threshold,
            issuance_timing,
            repository_retention,
            metrics,
            testbed,
        }
    }

    pub fn test(data_dir: &Path, enable_testbed: bool, enable_ca_refresh: bool, enable_suspend: bool) -> Self {
        Self::test_config(data_dir, enable_testbed, enable_ca_refresh, enable_suspend)
    }

    pub fn pubd_test(data_dir: &Path) -> Self {
        let mut config = Self::test_config(data_dir, false, false, false);
        config.port = 3001;
        config
    }

    pub fn get_config_filename() -> String {
        let matches = App::new(KRILL_SERVER_APP)
            .version(KRILL_VERSION)
            .arg(
                Arg::with_name("config")
                    .short("c")
                    .long("config")
                    .value_name("FILE")
                    .help("Override the path to the config file (default: './defaults/krill.conf')")
                    .required(false),
            )
            .get_matches();

        let config_file = matches.value_of("config").unwrap_or(KRILL_DEFAULT_CONFIG_FILE);

        config_file.to_string()
    }

    /// Creates the config (at startup). Panics in case of issues.
    pub fn create() -> Result<Self, ConfigError> {
        let config_file = Self::get_config_filename();

        let mut config = match Self::read_config(&config_file) {
            Err(e) => {
                if config_file == KRILL_DEFAULT_CONFIG_FILE {
                    Err(ConfigError::other(
                        "Cannot find config file. Please use --config to specify its location.",
                    ))
                } else {
                    Err(ConfigError::Other(format!(
                        "Error parsing config file: {}, error: {}",
                        config_file, e
                    )))
                }
            }
            Ok(config) => {
                config.init_logging()?;
                info!("{} uses configuration file: {}", KRILL_SERVER_APP, config_file);
                Ok(config)
            }
        }?;

        if config.ca_refresh_seconds < CA_REFRESH_SECONDS_MIN {
            warn!(
                "The value for 'ca_refresh_seconds' was below the minimum value, changing it to {} seconds",
                CA_REFRESH_SECONDS_MIN
            );
            config.ca_refresh_seconds = CA_REFRESH_SECONDS_MIN;
        }

        if config.ca_refresh_seconds > CA_REFRESH_SECONDS_MAX {
            warn!(
                "The value for 'ca_refresh_seconds' was above the maximum value, changing it to {} seconds",
                CA_REFRESH_SECONDS_MAX
            );
            config.ca_refresh_seconds = CA_REFRESH_SECONDS_MAX;
        }

        config
            .verify()
            .map_err(|e| ConfigError::Other(format!("Error parsing config file: {}, error: {}", config_file, e)))?;
        Ok(config)
    }

    pub fn verify(&self) -> Result<(), ConfigError> {
        if env::var(KRILL_ENV_ADMIN_TOKEN_DEPRECATED).is_ok() {
            warn!("The environment variable for setting the admin token has been updated from '{}' to '{}', please update as the old value may not be supported in future releases", KRILL_ENV_ADMIN_TOKEN_DEPRECATED, KRILL_ENV_ADMIN_TOKEN)
        }

        if self.port < 1024 {
            return Err(ConfigError::other("Port number must be >1024"));
        }

        if let Some(service_uri) = &self.service_uri {
            if !service_uri.as_str().ends_with('/') {
                return Err(ConfigError::other("service URI must end with '/'"));
            } else if service_uri.as_str().matches('/').count() != 3 {
                return Err(ConfigError::other(
                    "Service URI MUST specify a host name only, e.g. https://rpki.example.com:3000/",
                ));
            }
        }

        if self.issuance_timing.timing_publish_next_hours < 2 {
            return Err(ConfigError::other("timing_publish_next_hours must be at least 2"));
        }

        if self.issuance_timing.timing_publish_next_jitter_hours < 0 {
            return Err(ConfigError::other(
                "timing_publish_next_jitter_hours must be at least 0",
            ));
        }

        if self.issuance_timing.timing_publish_next_jitter_hours > (self.issuance_timing.timing_publish_next_hours / 2)
        {
            return Err(ConfigError::other(
                "timing_publish_next_jitter_hours must be at most timing_publish_next_hours divided by 2",
            ));
        }

        if self.issuance_timing.timing_publish_hours_before_next < 1 {
            return Err(ConfigError::other(
                "timing_publish_hours_before_next must be at least 1",
            ));
        }

        if self.issuance_timing.timing_publish_hours_before_next >= self.issuance_timing.timing_publish_next_hours {
            return Err(ConfigError::other(
                "timing_publish_hours_before_next must be smaller than timing_publish_hours",
            ));
        }

        if self.issuance_timing.timing_child_certificate_valid_weeks < 2 {
            return Err(ConfigError::other(
                "timing_child_certificate_valid_weeks must be at least 2",
            ));
        }

        if self.issuance_timing.timing_child_certificate_reissue_weeks_before < 1 {
            return Err(ConfigError::other(
                "timing_child_certificate_reissue_weeks_before must be at least 1",
            ));
        }

        if self.issuance_timing.timing_child_certificate_reissue_weeks_before
            >= self.issuance_timing.timing_child_certificate_valid_weeks
        {
            return Err(ConfigError::other("timing_child_certificate_reissue_weeks_before must be smaller than timing_child_certificate_valid_weeks"));
        }

        if self.issuance_timing.timing_roa_valid_weeks < 2 {
            return Err(ConfigError::other("timing_roa_valid_weeks must be at least 2"));
        }

        if self.issuance_timing.timing_roa_reissue_weeks_before < 1 {
            return Err(ConfigError::other("timing_roa_reissue_weeks_before must be at least 1"));
        }

        if self.issuance_timing.timing_roa_reissue_weeks_before >= self.issuance_timing.timing_roa_valid_weeks {
            return Err(ConfigError::other(
                "timing_roa_reissue_weeks_before must be smaller than timing_roa_valid_week",
            ));
        }

        if let Some(threshold) = self.suspend_child_after_inactive_hours {
            if threshold < CA_SUSPEND_MIN_HOURS {
                return Err(ConfigError::Other(format!(
                    "suspend_child_after_inactive_hours must be {} or higher (or not set at all)",
                    CA_SUSPEND_MIN_HOURS
                )));
            }
        }

        Ok(())
    }

    pub fn read_config(file: &str) -> Result<Self, ConfigError> {
        let mut v = Vec::new();
        let mut f =
            File::open(file).map_err(|e| KrillIoError::new(format!("Could not read open file '{}'", file), e))?;
        f.read_to_end(&mut v)
            .map_err(|e| KrillIoError::new(format!("Could not read config file '{}'", file), e))?;

        let c: Config = toml::from_slice(v.as_slice())?;
        Ok(c)
    }

    pub fn init_logging(&self) -> Result<(), ConfigError> {
        match self.log_type {
            LogType::File => self.file_logger(&self.log_file),
            LogType::Stderr => self.stderr_logger(),
            LogType::Syslog => {
                let facility = Facility::from_str(&self.syslog_facility)
                    .map_err(|_| ConfigError::other("Invalid syslog_facility"))?;
                self.syslog_logger(facility)
            }
        }
    }

    /// Creates a stderr logger.
    fn stderr_logger(&self) -> Result<(), ConfigError> {
        self.fern_logger()
            .chain(io::stderr())
            .apply()
            .map_err(|e| ConfigError::Other(format!("Failed to init stderr logging: {}", e)))
    }

    /// Creates a file logger using the file provided by `path`.
    fn file_logger(&self, path: &Path) -> Result<(), ConfigError> {
        let file = match fern::log_file(path) {
            Ok(file) => file,
            Err(err) => {
                let error_string = format!("Failed to open log file '{}': {}", path.display(), err);
                error!("{}", error_string.as_str());
                return Err(ConfigError::Other(error_string));
            }
        };
        self.fern_logger()
            .chain(file)
            .apply()
            .map_err(|e| ConfigError::Other(format!("Failed to init file logging: {}", e)))
    }

    /// Creates a syslog logger and configures correctly.
    #[cfg(unix)]
    fn syslog_logger(&self, facility: syslog::Facility) -> Result<(), ConfigError> {
        let process = env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| String::from("krill"));
        let pid = unsafe { libc::getpid() };
        let formatter = syslog::Formatter3164 {
            facility,
            hostname: None,
            process,
            pid,
        };
        let logger = syslog::unix(formatter.clone())
            .or_else(|_| syslog::tcp(formatter.clone(), ("127.0.0.1", 601)))
            .or_else(|_| syslog::udp(formatter, ("127.0.0.1", 0), ("127.0.0.1", 514)));
        match logger {
            Ok(logger) => self
                .fern_logger()
                .chain(logger)
                .apply()
                .map_err(|e| ConfigError::Other(format!("Failed to init syslog: {}", e))),
            Err(err) => {
                let msg = format!("Cannot connect to syslog: {}", err);
                Err(ConfigError::Other(msg))
            }
        }
    }

    /// Creates and returns a fern logger with log level tweaks
    fn fern_logger(&self) -> fern::Dispatch {
        // suppress overly noisy logging
        let framework_level = self.log_level.min(LevelFilter::Warn);
        let krill_framework_level = self.log_level.min(LevelFilter::Debug);

        // disable Oso logging unless the Oso specific POLAR_LOG environment
        // variable is set, it's too noisy otherwise
        let oso_framework_level = if env::var("POLAR_LOG").is_ok() {
            self.log_level.min(LevelFilter::Trace)
        } else {
            self.log_level.min(LevelFilter::Info)
        };

        let show_target = self.log_level == LevelFilter::Trace || self.log_level == LevelFilter::Debug;
        fern::Dispatch::new()
            .format(move |out, message, record| {
                if show_target {
                    out.finish(format_args!(
                        "{} [{}] [{}] {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                        record.level(),
                        record.target(),
                        message
                    ))
                } else {
                    out.finish(format_args!(
                        "{} [{}] {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                        record.level(),
                        message
                    ))
                }
            })
            .level(self.log_level)
            .level_for("rustls", framework_level)
            .level_for("hyper", framework_level)
            .level_for("mio", framework_level)
            .level_for("reqwest", framework_level)
            .level_for("tokio_reactor", framework_level)
            .level_for("tokio_util::codec::framed_read", framework_level)
            .level_for("want", framework_level)
            .level_for("tracing::span", framework_level)
            .level_for("h2", framework_level)
            .level_for("oso", oso_framework_level)
            .level_for("krill::commons::eventsourcing", krill_framework_level)
            .level_for("krill::commons::util::file", krill_framework_level)
    }
}

#[derive(Debug)]
pub enum ConfigError {
    IoError(KrillIoError),
    TomlError(toml::de::Error),
    RpkiUriError(uri::Error),
    Other(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::IoError(e) => e.fmt(f),
            ConfigError::TomlError(e) => e.fmt(f),
            ConfigError::RpkiUriError(e) => e.fmt(f),
            ConfigError::Other(s) => s.fmt(f),
        }
    }
}

impl ConfigError {
    pub fn other(s: &str) -> ConfigError {
        ConfigError::Other(s.to_string())
    }
}

impl From<KrillIoError> for ConfigError {
    fn from(e: KrillIoError) -> Self {
        ConfigError::IoError(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::TomlError(e)
    }
}

impl From<uri::Error> for ConfigError {
    fn from(e: uri::Error) -> Self {
        ConfigError::RpkiUriError(e)
    }
}

//------------ LogType -------------------------------------------------------

/// The target to log to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogType {
    Stderr,
    File,
    Syslog,
}

impl<'de> Deserialize<'de> for LogType {
    fn deserialize<D>(d: D) -> Result<LogType, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string = String::deserialize(d)?;
        match string.as_str() {
            "stderr" => Ok(LogType::Stderr),
            "file" => Ok(LogType::File),
            "syslog" => Ok(LogType::Syslog),
            _ => Err(de::Error::custom(format!(
                "expected \"stderr\" or \"file\", found : \"{}\"",
                string
            ))),
        }
    }
}

//------------ HttpsMode -----------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpsMode {
    Existing,
    Generate,
}

impl<'de> Deserialize<'de> for HttpsMode {
    fn deserialize<D>(d: D) -> Result<HttpsMode, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string = String::deserialize(d)?;
        match string.as_str() {
            "existing" => Ok(HttpsMode::Existing),
            "generate" => Ok(HttpsMode::Generate),
            _ => Err(de::Error::custom(format!(
                "expected \"existing\", or \"generate\", \
                 found: \"{}\"",
                string
            ))),
        }
    }
}

//------------ AuthType -----------------------------------------------------

/// The target to log to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthType {
    AdminToken,
    #[cfg(feature = "multi-user")]
    ConfigFile,
    #[cfg(feature = "multi-user")]
    OpenIDConnect,
}

impl<'de> Deserialize<'de> for AuthType {
    fn deserialize<D>(d: D) -> Result<AuthType, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string = String::deserialize(d)?;
        match string.as_str() {
            "admin-token" => Ok(AuthType::AdminToken),
            #[cfg(feature = "multi-user")]
            "config-file" => Ok(AuthType::ConfigFile),
            #[cfg(feature = "multi-user")]
            "openid-connect" => Ok(AuthType::OpenIDConnect),
            _ => {
                #[cfg(not(feature = "multi-user"))]
                let msg = format!("expected \"admin-token\", found: \"{}\"", string);
                #[cfg(feature = "multi-user")]
                let msg = format!(
                    "expected \"config-file\", \"admin-token\", or \"openid-connect\", found: \"{}\"",
                    string
                );
                Err(de::Error::custom(msg))
            }
        }
    }
}

//------------ Tests ---------------------------------------------------------

#[cfg(test)]
mod tests {

    use crate::test;
    use std::env;

    use super::*;

    #[test]
    fn should_parse_default_config_file() {
        // Config for auth token is required! If there is nothing in the conf
        // file, then an environment variable must be set.
        env::set_var(KRILL_ENV_ADMIN_TOKEN, "secret");

        let c = Config::read_config("./defaults/krill.conf").unwrap();
        let expected_socket_addr: SocketAddr = ([127, 0, 0, 1], 3000).into();
        assert_eq!(c.socket_addr(), expected_socket_addr);
        assert!(c.testbed().is_none());
    }

    #[test]
    fn should_parse_testbed_config_file() {
        // Config for auth token is required! If there is nothing in the conf
        // file, then an environment variable must be set.
        env::set_var(KRILL_ENV_ADMIN_TOKEN, "secret");

        let c = Config::read_config("./defaults/krill-testbed.conf").unwrap();

        let testbed = c.testbed().unwrap();
        assert_eq!(testbed.ta_aia(), &test::rsync("rsync://testbed.example.com/ta/ta.cer"));
        assert_eq!(testbed.ta_uri(), &test::https("https://testbed.example.com/ta/ta.cer"));

        let uris = testbed.publication_server_uris();
        assert_eq!(uris.rrdp_base_uri(), &test::https("https://testbed.example.com/rrdp/"));
        assert_eq!(uris.rsync_jail(), &test::rsync("rsync://testbed.example.com/repo/"));
    }

    #[test]
    fn should_set_correct_log_levels() {
        use log::Level as LL;

        fn void_logger_from_krill_config(config_bytes: &[u8]) -> Box<dyn log::Log> {
            let c: Config = toml::from_slice(config_bytes).unwrap();
            let void_output = fern::Output::writer(Box::new(io::sink()), "");
            let (_, void_logger) = c.fern_logger().chain(void_output).into_log();
            void_logger
        }

        fn for_target_at_level(target: &str, level: LL) -> log::Metadata {
            log::Metadata::builder().target(target).level(level).build()
        }

        fn should_logging_be_enabled_at_this_krill_config_log_level(log_level: &LL, config_level: &str) -> bool {
            let log_level_from_krill_config_level = LL::from_str(config_level).unwrap();
            log_level <= &log_level_from_krill_config_level
        }

        // Krill requires an auth token to be defined, give it one in the environment
        env::set_var(KRILL_ENV_ADMIN_TOKEN, "secret");

        // Define sets of log targets aka components of Krill that we want to test log settings for, based on the
        // rules & exceptions that the actual code under test is supposed to configure the logger with
        let krill_components = vec!["krill"];
        let krill_framework_components = vec!["krill::commons::eventsourcing", "krill::commons::util::file"];
        let other_key_components = vec!["hyper", "reqwest", "oso"];

        let krill_key_components = vec![krill_components, krill_framework_components.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let all_key_components = vec![krill_key_components.clone(), other_key_components]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();

        //
        // Test that important log levels are enabled for all key components
        //

        // for each important Krill config log level
        for config_level in &["error", "warn"] {
            // build a logger for that config
            let log = void_logger_from_krill_config(format!(r#"log_level = "{}""#, config_level).as_bytes());

            // for all log levels
            for log_msg_level in &[LL::Error, LL::Warn, LL::Info, LL::Debug, LL::Trace] {
                // determine if logging should be enabled or not
                let should_be_enabled =
                    should_logging_be_enabled_at_this_krill_config_log_level(log_msg_level, config_level);

                // for each Krill component we want to pretend to log as
                for component in &all_key_components {
                    // verify that logging is enabled or not as expected
                    assert_eq!(
                        should_be_enabled,
                        log.enabled(&for_target_at_level(component, *log_msg_level)),
                        // output an easy to understand test failure description
                        "Logging at level {} with log_level={} should be {} for component {}",
                        log_msg_level,
                        config_level,
                        if should_be_enabled { "enabled" } else { "disabled" },
                        component
                    );
                }
            }
        }

        //
        // Test that info level and below are only enabled for Krill at the right log levels
        //

        // for each Krill config log level we want to test
        for config_level in &["info", "debug", "trace"] {
            // build a logger for that config
            let log = void_logger_from_krill_config(format!(r#"log_level = "{}""#, config_level).as_bytes());

            // for each level of interest that messages could be logged at
            for log_msg_level in &[LL::Info, LL::Debug, LL::Trace] {
                // determine if logging should be enabled or not
                let should_be_enabled =
                    should_logging_be_enabled_at_this_krill_config_log_level(log_msg_level, config_level);

                // for each Krill component we want to pretend to log as
                for component in &krill_key_components {
                    // framework components shouldn't log at Trace level
                    let should_be_enabled = should_be_enabled
                        && (*log_msg_level < LL::Trace || !krill_framework_components.contains(component));

                    // verify that logging is enabled or not as expected
                    assert_eq!(
                        should_be_enabled,
                        log.enabled(&for_target_at_level(component, *log_msg_level)),
                        // output an easy to understand test failure description
                        "Logging at level {} with log_level={} should be {} for component {}",
                        log_msg_level,
                        config_level,
                        if should_be_enabled { "enabled" } else { "disabled" },
                        component
                    );
                }
            }
        }

        //
        // Test that Oso logging at levels below Info is only enabled if the Oso POLAR_LOG=1
        // environment variable is set
        //
        let component = "oso";
        for set_polar_log_env_var in &[true, false] {
            // setup env vars
            if *set_polar_log_env_var {
                env::set_var("POLAR_LOG", "1");
            } else {
                env::remove_var("POLAR_LOG");
            }

            // for each Krill config log level we want to test
            for config_level in &["debug", "trace"] {
                // build a logger for that config
                let log = void_logger_from_krill_config(format!(r#"log_level = "{}""#, config_level).as_bytes());

                // for each level of interest that messages could be logged at
                for log_msg_level in &[LL::Debug, LL::Trace] {
                    // determine if logging should be enabled or not
                    let should_be_enabled =
                        should_logging_be_enabled_at_this_krill_config_log_level(log_msg_level, config_level)
                            && *set_polar_log_env_var;

                    // verify that logging is enabled or not as expected
                    assert_eq!(
                        should_be_enabled,
                        log.enabled(&for_target_at_level(component, *log_msg_level)),
                        // output an easy to understand test failure description
                        r#"Logging at level {} with log_level={} should be {} for component {} and env var POLAR_LOG is {}"#,
                        log_msg_level,
                        config_level,
                        if should_be_enabled { "enabled" } else { "disabled" },
                        component,
                        if *set_polar_log_env_var { "set" } else { "not set" }
                    );
                }
            }
        }
    }

    #[test]
    fn config_should_accept_and_warn_about_auth_token() {
        let old_config = b"auth_token = \"secret\"";

        let c: Config = toml::from_slice(old_config).unwrap();
        assert_eq!(c.admin_token.as_ref(), "secret");
    }
}
