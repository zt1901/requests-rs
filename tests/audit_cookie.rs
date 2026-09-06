use wreq::cookie::{CookieStore, Cookies, Jar};
use http::{Uri, Version};
fn values(jar: &Jar, url: &str) -> String {
    match jar.cookies(&url.parse::<Uri>().unwrap(), Version::HTTP_11) {
        Cookies::Compressed(v) => v.to_str().unwrap().to_owned(),
        Cookies::Uncompressed(v) => v.iter().map(|x| x.to_str().unwrap()).collect::<Vec<_>>().join("; "),
        _ => String::new(),
    }
}
#[test]
fn host_only_is_not_shared_with_subdomains() {
    let j=Jar::default(); j.add("host=1; Path=/", "https://example.test/");
    assert_eq!(values(&j,"https://example.test/"),"host=1");
    assert_eq!(values(&j,"https://sub.example.test/"),"");
    j.add("domain=1; Domain=example.test; Path=/","https://example.test/");
    assert_eq!(values(&j,"https://sub.example.test/"),"domain=1");
}
#[test]
fn max_age_expires_without_refreshing_on_read() {
    let j=Jar::default();j.add("short=1; Max-Age=1; Path=/","https://example.test/");
    assert_eq!(values(&j,"https://example.test/"),"short=1");
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert_eq!(values(&j,"https://example.test/"),"");
}
#[test]
fn max_age_overrides_expires_and_negative_age_deletes() {
    let j=Jar::default();j.add("x=1; Max-Age=3600; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/","https://example.test/");
    assert_eq!(values(&j,"https://example.test/"),"x=1");
    j.add("x=2; Max-Age=-1; Expires=Fri, 01 Jan 9999 00:00:00 GMT; Path=/","https://example.test/");
    assert_eq!(values(&j,"https://example.test/"),"");
}
