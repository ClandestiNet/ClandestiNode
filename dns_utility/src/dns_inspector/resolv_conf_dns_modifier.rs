// Copyright (c) 2017-2019, Substratum LLC (https://substratum.net) and/or its affiliates. All rights reserved.
#![cfg(target_os = "linux")]
use crate::dns_inspector::dns_modifier::DnsModifier;
use regex::Regex;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::net::{IpAddr, AddrParseError};
use std::str::FromStr;
use crate::dns_inspector::DnsInspectionError;

pub struct ResolvConfDnsModifier {
    root: PathBuf,
}

impl DnsModifier for ResolvConfDnsModifier {
    fn type_name(&self) -> &'static str {
        "ResolvConfDnsModifier"
    }

    #[allow(unused_mut)]
    fn inspect(&self) -> Result<Vec<IpAddr>, DnsInspectionError> {
        let (_, contents) = self.open_resolv_conf(false)?;
        self.inspect_contents(contents)
    }
}

impl Default for ResolvConfDnsModifier {
    fn default() -> Self {
        Self::new()
    }
}

impl ResolvConfDnsModifier {
    pub fn new() -> ResolvConfDnsModifier {
        ResolvConfDnsModifier {
            root: PathBuf::from("/"),
        }
    }

    fn open_resolv_conf(&self, for_write: bool) -> Result<(File, String), DnsInspectionError> {
        let mut open_options = OpenOptions::new();
        open_options.read(true);
        open_options.write(for_write);
        open_options.create(false);
        let path = Path::new(&self.root)
            .join(Path::new("etc"))
            .join(Path::new("resolv.conf"));
        let mut file = match open_options.open(path.clone()) {
            Ok(f) => f,
            Err(ref e) if e.kind() == ErrorKind::NotFound => {
                return Err(DnsInspectionError::InvalidConfigFile(ResolvConfDnsModifier::process_msg(
                    "/etc/resolv.conf was not found",
                    for_write,
                )));
            }
            Err(ref e) if e.kind() == ErrorKind::PermissionDenied => {
                let suffix = if for_write { " and writable" } else { "" };
                let msg = format!("/etc/resolv.conf is not readable{}", suffix);
                return Err(DnsInspectionError::InvalidConfigFile(ResolvConfDnsModifier::process_msg(
                    msg.as_str(),
                    for_write
                )));
            }
            Err(ref e) if e.raw_os_error() == Some(21) => {
                return Err(DnsInspectionError::InvalidConfigFile(ResolvConfDnsModifier::process_msg(
                    "/etc/resolv.conf is a directory",
                    for_write,
                )));
            }
            Err(e) => return Err(DnsInspectionError::InvalidConfigFile(
                format!("Unexpected error opening {:?}: {}", path, e)
            )),
        };
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_err() {
            return Err(DnsInspectionError::InvalidConfigFile(ResolvConfDnsModifier::process_msg(
                "/etc/resolv.conf is not a UTF-8 text file",
                for_write,
            )));
        }
        Ok((file, contents))
    }

    fn process_msg(msg: &str, for_write: bool) -> String {
        if for_write {
            format!("{} and could not be modified", msg)
        } else {
            msg.to_string()
        }
    }

    fn inspect_contents(
        &self,
        contents: String,
    ) -> Result<Vec<IpAddr>, DnsInspectionError> {
        let active_nameservers = self.active_nameservers(&contents[..]);
        type Triple = (Option<String>, Option<IpAddr>, Option<DnsInspectionError>);
        let (successes, mut failures): (Vec<Triple>, Vec<Triple>) = active_nameservers
            .into_iter()
            .map (|(nameserver_line, _)| {
                match self.nameserver_line_to_ip_str(nameserver_line) {
                    Ok(ip_str) => match IpAddr::from_str(&ip_str) {
                        Ok(ip_addr) => (Some (ip_str), Some(ip_addr), None),
                        Err (e) => (Some (ip_str.clone()), None, Some (DnsInspectionError::BadEntryFormat(ip_str))),
                    },
                    Err(e) => (None, None, Some (e))
                }
            })
            .partition(|(_, _, err_opt)| err_opt.is_none());
        let ip_vec: Vec<IpAddr> = match (successes, failures) {
            (s, f) if s.is_empty() && f.is_empty() => return Err(DnsInspectionError::NotConnected),
            (s, mut f) if s.is_empty() => return Err(f.remove(0).2.expect("partition() isn't working")),
            (s, _) => s
                .into_iter()
                .flat_map(|(_, ip_addr_opt, _)| ip_addr_opt)
                .collect(),
        };
        self.check_disconnected(&ip_vec)?;
        Ok(ip_vec)
    }

    pub fn nameserver_line_to_ip_str(&self, nameserver_line: String) -> Result<String, DnsInspectionError> {
        let regex = Regex::new(r"^\s*nameserver\s+([^\s#]*)").expect("Regex syntax error");
        let captures = match regex.captures(nameserver_line.as_str()) {
            Some(cap) => cap,
            None => return Err(DnsInspectionError::BadEntryFormat(nameserver_line))
        };
        Ok(String::from(
            captures
                .get(1)
                .expect("Regex had no capture group")
                .as_str(),
        ))
    }

    pub fn active_nameservers(&self, contents: &str) -> Vec<(String, usize)> {
        let regex = Regex::new(r"(^|\n)\s*(nameserver\s+[^\s]*)").expect("Regex syntax error");
        let capture_matches = regex.captures_iter(contents);
        capture_matches
            .map(|captures| {
                let capture = captures.get(2).expect("Inconsistent regex code");
                (String::from(capture.as_str()), capture.start())
            })
            .collect()
    }

    fn check_disconnected(&self, active_nameservers: &Vec<IpAddr>) -> Result<(), DnsInspectionError> {
        if active_nameservers.is_empty() {
            Err(DnsInspectionError::NotConnected)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::io::{Write, SeekFrom, Seek};
    use std::os::unix::fs::PermissionsExt;
    use std::net::IpAddr;
    use std::str::FromStr;

    #[test]
    fn nameserver_line_to_ip_complains_when_given_badly_formatted_nameserver_line() {
        let nameserver_line = "booga-booga".to_string();
        let subject = ResolvConfDnsModifier::new();

        let result = subject.nameserver_line_to_ip_str(nameserver_line);

        assert_eq! (result, Err(DnsInspectionError::BadEntryFormat("booga-booga".to_string())))
    }

    #[test]
    fn nameserver_line_to_ip_handles_line_with_leading_whitespace_and_comment() {
        let nameserver_line =
            "  \t  \tnameserver  \t  \t booga-booga  \t\t  # comment #".to_string();
        let subject = ResolvConfDnsModifier::new();

        let result = subject.nameserver_line_to_ip_str(nameserver_line);

        assert_eq!(result, Ok("booga-booga".to_string()));
    }

    #[test]
    fn nameserver_line_to_ip_handles_line_with_minimum_whitespace_and_no_comment() {
        let nameserver_line = "nameserver booga-booga".to_string();
        let subject = ResolvConfDnsModifier::new();

        let result = subject.nameserver_line_to_ip_str(nameserver_line);

        assert_eq!(result, Ok("booga-booga".to_string()));
    }

    #[test]
    fn active_nameservers_are_properly_detected_in_trimmed_file() {
        let contents =
            "nameserver beginning\n#nameserver commented\n# nameserver commented2\n nameserver preceded_by_space\nnameserver followed_by_space \nnameserver with more than two words\n ## nameserver double_comment\nnameserver ending";
        let subject = ResolvConfDnsModifier::new();

        let result = subject.active_nameservers(contents);

        assert_eq!(
            result,
            vec!(
                (String::from("nameserver beginning"), 0),
                (String::from("nameserver preceded_by_space"), 68),
                (String::from("nameserver followed_by_space"), 97),
                (String::from("nameserver with"), 127),
                (String::from("nameserver ending"), 193)
            )
        );
    }

    #[test]
    fn active_nameservers_are_properly_detected_in_untrimmed_file() {
        let contents =
            "#leading comment\nnameserver beginning\nnameserver ending\n#trailing comment";
        let subject = ResolvConfDnsModifier::new();

        let result = subject.active_nameservers(contents);

        assert_eq!(
            result,
            vec!(
                (String::from("nameserver beginning"), 17),
                (String::from("nameserver ending"), 38)
            )
        );
    }

    #[test]
    fn instance_knows_its_type_name() {
        let subject = ResolvConfDnsModifier::new();

        let result = subject.type_name();

        assert_eq!(result, "ResolvConfDnsModifier");
    }

    #[test]
    fn inspect_complains_if_resolv_conf_does_not_exist() {
        let root = make_root("inspect_complains_if_resolv_conf_does_not_exist");
        let mut subject = ResolvConfDnsModifier::new();
        subject.root = root;

        let result = subject.inspect();

        assert_eq!(
            result.err().unwrap(),
            DnsInspectionError::InvalidConfigFile(String::from("/etc/resolv.conf was not found"))
        );
    }

    #[test]
    fn inspect_complains_if_resolv_conf_exists_but_is_a_directory() {
        let root = make_root("inspect_complains_if_resolv_conf_exists_but_is_a_directory");
        fs::create_dir_all(
            Path::new(&root)
                .join(Path::new("etc"))
                .join(Path::new("resolv.conf")),
        )
        .unwrap();
        let mut subject = ResolvConfDnsModifier::new();
        subject.root = root;

        let result = subject.inspect();

        assert_eq!(
            result.err().unwrap(),
            DnsInspectionError::InvalidConfigFile(String::from("/etc/resolv.conf is not a UTF-8 text file"))
        );
    }

    #[test]
    fn inspect_complains_if_resolv_conf_exists_but_is_not_readable() {
        let root = make_root("inspect_complains_if_resolv_conf_exists_but_is_not_readable");
        let file = make_resolv_conf(&root, "");
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_mode(0o333);
        file.set_permissions(permissions).unwrap();
        let mut subject = ResolvConfDnsModifier::new();
        subject.root = root;

        let result = subject.inspect();

        assert_eq!(
            result.err().unwrap(),
            DnsInspectionError::InvalidConfigFile(String::from("/etc/resolv.conf is not readable"))
        );
    }

    #[test]
    fn inspect_complains_if_resolv_conf_is_not_utf_8() {
        let root = make_root("inspect_complains_if_resolv_conf_is_not_utf_8");
        let mut file = make_resolv_conf(&root, "");
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write(&[192, 193]).unwrap();
        let mut subject = ResolvConfDnsModifier::new();
        subject.root = root;

        let result = subject.inspect();

        assert_eq!(
            result.err().unwrap(),
            DnsInspectionError::InvalidConfigFile("/etc/resolv.conf is not a UTF-8 text file".to_string())
        );
    }

    #[test]
    fn inspect_complains_if_there_is_no_preexisting_nameserver_directive() {
        let root = make_root("inspect_complains_if_there_is_no_preexisting_nameserver_directive");
        make_resolv_conf(&root, "");
        let mut subject = ResolvConfDnsModifier::new();
        subject.root = root;

        let result = subject.inspect();

        assert_eq!(
            result.err().unwrap(),
            DnsInspectionError::NotConnected
        );
    }

    #[test]
    fn inspect_complains_if_nameserver_directive_has_bad_ip_address() {
        let root = make_root("inspect_complains_if_there_is_no_preexisting_nameserver_directive");
        make_resolv_conf(&root, "nameserver 300.301.302.303");
        let mut subject = ResolvConfDnsModifier::new();
        subject.root = root;

        let result = subject.inspect();

        assert_eq!(
            result.err().unwrap(),
            DnsInspectionError::BadEntryFormat("300.301.302.303".to_string())
        );
    }

    #[test]
    fn inspect_works_if_everything_is_copacetic() {
        let root = make_root("inspect_works_if_everything_is_copacetic");
        make_resolv_conf (&root, "#comment\n## nameserver 1.1.1.1\nnameserver 8.8.8.8\nnameserver 2603:6011:b504:bf01:2ad:24ff:fe57:fd78\n#nameserver 127.0.0.1\n");
        let mut subject = ResolvConfDnsModifier::new();
        subject.root = root.clone();

        let result = subject.inspect().unwrap();

        assert_eq! (result, vec![
            IpAddr::from_str("8.8.8.8").unwrap(),
            IpAddr::from_str("2603:6011:b504:bf01:2ad:24ff:fe57:fd78").unwrap(),
        ]);
    }

    fn make_root(test_name: &str) -> PathBuf {
        let cur_dir = env::current_dir().unwrap();
        let generated_dir = cur_dir.join(Path::new("generated"));
        let suite_dir = generated_dir.join(Path::new("ResolvConfDnsModifier"));
        let base_dir = suite_dir.join(Path::new(test_name));
        let _ = fs::remove_dir_all(base_dir.clone()); // don't care if it doesn't exist
        fs::create_dir_all(base_dir.clone()).unwrap();
        base_dir
    }

    fn make_resolv_conf(root: &PathBuf, file_contents: &str) -> File {
        let path = Path::new(root).join(Path::new("etc"));
        fs::create_dir_all(path.clone()).unwrap();
        let mut file = File::create(path.join(Path::new("resolv.conf"))).unwrap();
        write!(file, "{}", file_contents).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file
    }
}
