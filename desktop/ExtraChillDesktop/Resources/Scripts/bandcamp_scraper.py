#!/usr/bin/env python3
"""
Bandcamp Email Scraper (Playwright Edition)

Scrapes Bandcamp Discover for album URLs and extracts artist contact emails.
Uses Playwright for JS rendering (no chromedriver dependency).

Usage:
    python bandcamp_scraper.py --tag "south-carolina" --clicks 3 --output json

Output:
    - stderr: Real-time progress logs
    - stdout: Final JSON result
"""

import argparse
import json
import re
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from urllib.parse import urljoin

import requests
import tldextract
from bs4 import BeautifulSoup
from playwright.sync_api import sync_playwright, TimeoutError as PlaywrightTimeout

# Constants
UA = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/124.0.0.0 Safari/537.36"
)

HEADERS = {
    'User-Agent': UA,
    'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
    'Accept-Language': 'en-US,en;q=0.9',
}

PUBLIC_EMAIL_DOMAINS = {
    # Google
    "gmail.com", "googlemail.com",
    # Microsoft
    "outlook.com", "hotmail.com", "live.com", "msn.com",
    # Apple
    "icloud.com", "me.com", "mac.com",
    # Yahoo
    "yahoo.com", "ymail.com", "rocketmail.com",
    # Privacy-focused
    "protonmail.com", "proton.me", "tutanota.com", "tutamail.com",
    # Other major providers
    "aol.com", "mail.com", "zoho.com", "fastmail.com", "hey.com",
    # Indie/activist
    "riseup.net", "disroot.org",
}

EXCLUDED_DOMAINS = {
    # Social platforms
    "instagram.com", "facebook.com", "twitter.com", "x.com",
    "youtube.com", "tiktok.com", "soundcloud.com", "twitch.tv",
    "reverbnation.com", "spotify.com", "tumblr.com", "bandcamp.com",
    "bsky.app", "vk.com", "genius.com", "mixcloud.com", "discogs.com",
    # Storefronts (no artist emails)
    "redbubble.com", "bigcartel.com", "storenvy.com", "limitedrun.com",
    # Platforms without artist contact info
    "patreon.com", "linktr.ee", "bio.link", "beacons.ai",
    # App stores
    "play.google.com", "apps.apple.com", "itunes.apple.com",
}


def log(message: str) -> None:
    """Log to stderr for real-time progress."""
    print(message, file=sys.stderr, flush=True)


# Strict email validation: ASCII only, valid TLD (2+ letters)
EMAIL_REGEX = re.compile(r'^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$')


def is_valid_email(email: str) -> bool:
    """Validate email format strictly (ASCII only, proper TLD)."""
    if not email or len(email) > 254:
        return False
    return EMAIL_REGEX.match(email) is not None


def extract_emails(text: str) -> list[str]:
    """Extract emails including de-obfuscated [at]/[dot] forms."""
    normal = re.findall(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", text)
    obfus = re.findall(
        r"([A-Za-z0-9._%+-]+)\s?\[at\]\s?([A-Za-z0-9.-]+)\s?\[dot\]\s?([A-Za-z.]{2,})",
        text,
        flags=re.I,
    )
    deobfus = [f"{u}@{d}.{t}" for u, d, t in obfus]
    # Filter through strict validation
    all_emails = list(set(normal + deobfus))
    return [e for e in all_emails if is_valid_email(e)]


def extract_mailto_emails(soup: BeautifulSoup) -> list[str]:
    """Extract emails from mailto: links in the page."""
    emails = []
    for a in soup.find_all("a", href=True):
        href = a.get("href", "")
        if href.lower().startswith("mailto:"):
            # Remove "mailto:" prefix and any query params (?subject=...)
            email = href[7:].split("?")[0].strip()
            if is_valid_email(email):
                emails.append(email)
    return list(set(emails))


def filter_emails_by_domain(emails: list[str], website_domain: str) -> list[str]:
    """Keep emails matching website domain or from public providers."""
    kept = []
    skipped = []
    for email in emails:
        if '@' not in email:
            continue
        email_domain = email.split('@')[-1].lower()
        extracted = tldextract.extract(email_domain).top_domain_under_public_suffix or email_domain
        
        if website_domain and extracted == website_domain.lower():
            kept.append(email)
        elif extracted in PUBLIC_EMAIL_DOMAINS:
            kept.append(email)
        else:
            skipped.append(email)
    
    if skipped:
        log(f"    Skipped email(s) (domain mismatch): {skipped}")
    
    return kept


def discover_album_urls(tag: str, clicks: int, headless: bool) -> list[str]:
    """Use Playwright to gather album URLs from Bandcamp's discover page."""
    if tag:
        log(f"Discovering albums for tag '{tag}'...")
        discover_url = f"https://bandcamp.com/discover/{tag}?s=rand"
    else:
        log("Discovering albums from generic discover page...")
        discover_url = "https://bandcamp.com/discover?s=rand"
    
    collected_links = set()
    selector = "li.results-grid-item div.meta a"
    
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=headless)
        context = browser.new_context(user_agent=UA)
        page = context.new_page()
        
        log("Loading discover page...")
        page.goto(discover_url, wait_until="networkidle")
        time.sleep(1)
        
        # Accept cookies if present
        try:
            page.click("button:has-text('Accept')", timeout=3000)
            log("Cookies accepted")
        except PlaywrightTimeout:
            pass
        
        # Initial scrape
        log("Scraping initially visible albums...")
        elements = page.query_selector_all(selector)
        for el in elements:
            href = el.get_attribute("href")
            if href:
                collected_links.add(href.split("?")[0])
        log(f"Found {len(collected_links)} initial album URLs")
        
        # Click "View more" and scrape
        log(f"Clicking 'View more' up to {clicks} times...")
        for i in range(clicks):
            try:
                view_more = page.query_selector("#view-more")
                if not view_more:
                    log("No more 'View more' button found")
                    break
                
                view_more.scroll_into_view_if_needed()
                view_more.click()
                log(f"Click {i + 1}/{clicks} - waiting for content...")
                time.sleep(1.5)
                
                # Scrape new links
                elements = page.query_selector_all(selector)
                new_count = 0
                for el in elements:
                    href = el.get_attribute("href")
                    if href:
                        cleaned = href.split("?")[0]
                        if cleaned not in collected_links:
                            collected_links.add(cleaned)
                            new_count += 1
                log(f"Found {new_count} new album URLs")
                
            except Exception as e:
                log(f"Error during click {i + 1}: {e}")
                break
        
        browser.close()
    
    log(f"Total: {len(collected_links)} unique album URLs collected")
    return list(collected_links)


def scrape_album_page(url: str, session: requests.Session) -> tuple[list[str], str, str, str, list[str], bool]:
    """Scrape an album page for emails and external links. Returns (emails, ext_url, artist_name, bio, logs, rate_limited)."""
    logs = []
    logs.append(f"Scraping album: {url}")
    
    try:
        r = session.get(url, timeout=15)
        if r.status_code == 429:
            logs.append("  Rate limited by Bandcamp - backing off...")
            return [], "", "", "", logs, True
        r.raise_for_status()
    except Exception as e:
        logs.append(f"  Request failed: {e}")
        return [], "", "", "", logs, False
    
    soup = BeautifulSoup(r.text, "html.parser")
    
    # Artist name
    name_el = soup.select_one("p#band-name-location span.title")
    artist_name = name_el.get_text(strip=True) if name_el else ""
    
    # Bio text
    bio_text = ""
    for sel in ("#bio-text", ".peekaboo-text", ".tralbumData.truncated"):
        el = soup.select_one(sel)
        if el:
            bio_text += " " + el.get_text(" ", strip=True)
    
    # Extract emails from bio text
    emails = extract_emails(bio_text)
    if emails:
        logs.append(f"  Found email(s) in bio: {emails}")
        return emails, "", artist_name, bio_text.strip(), logs, False
    
    # Extract emails from mailto: links
    mailto_emails = extract_mailto_emails(soup)
    if mailto_emails:
        logs.append(f"  Found email(s) in mailto links: {mailto_emails}")
        return mailto_emails, "", artist_name, bio_text.strip(), logs, False
    
    logs.append("  No email in bio or mailto links, checking external links...")
    
    # Find external website link
    external_link = ""
    for a in soup.select("#band-links a"):
        href = a.get("href", "")
        if not href.startswith("http"):
            continue
        
        try:
            domain = tldextract.extract(href).top_domain_under_public_suffix
            if domain and domain not in EXCLUDED_DOMAINS:
                external_link = href
                logs.append(f"  Found external link: {external_link}")
                break
        except Exception:
            continue
    
    return [], external_link, artist_name, bio_text.strip(), logs, False


def scrape_external_site(url: str, session: requests.Session, logs: list[str]) -> list[str]:
    """Scrape an external website for contact emails. Appends to logs list."""
    logs.append(f"  Visiting external site: {url}")
    
    try:
        website_domain = tldextract.extract(url).top_domain_under_public_suffix
        if website_domain in EXCLUDED_DOMAINS:
            return []
    except Exception:
        website_domain = ""
    
    try:
        r = session.get(url, timeout=15, allow_redirects=True)
        r.raise_for_status()
    except Exception as e:
        logs.append(f"    Request failed: {e}")
        return []
    
    soup = BeautifulSoup(r.text, "html.parser")
    for tag in soup(["script", "style", "noscript"]):
        tag.decompose()
    
    text = soup.get_text(" ", strip=True)
    emails = extract_emails(text)
    
    if emails:
        filtered = filter_emails_by_domain(emails, website_domain)
        if filtered:
            logs.append(f"    Found email(s) in text: {filtered}")
            return filtered
    
    # Check mailto: links on main page
    mailto_emails = extract_mailto_emails(soup)
    if mailto_emails:
        filtered = filter_emails_by_domain(mailto_emails, website_domain)
        if filtered:
            logs.append(f"    Found email(s) in mailto links: {filtered}")
            return filtered
    
    # Try contact page
    logs.append("    Checking for contact page...")
    for a in soup.find_all("a", href=True):
        link_text = a.get_text(strip=True).lower()
        link_href = a["href"].lower()
        
        if "contact" in link_text or "contact" in link_href:
            try:
                contact_url = urljoin(r.url, a["href"])
                if not contact_url.startswith("http"):
                    continue
                
                logs.append(f"    Found contact page: {contact_url}")
                contact_r = session.get(contact_url, timeout=15)
                contact_r.raise_for_status()
                
                contact_soup = BeautifulSoup(contact_r.text, "html.parser")
                for tag in contact_soup(["script", "style", "noscript"]):
                    tag.decompose()
                
                contact_text = contact_soup.get_text(" ", strip=True)
                contact_emails = extract_emails(contact_text)
                
                if contact_emails:
                    filtered = filter_emails_by_domain(contact_emails, website_domain)
                    if filtered:
                        logs.append(f"    Found email(s) on contact page (text): {filtered}")
                        return filtered
                
                # Check mailto: links on contact page
                contact_mailto = extract_mailto_emails(contact_soup)
                if contact_mailto:
                    filtered = filter_emails_by_domain(contact_mailto, website_domain)
                    if filtered:
                        logs.append(f"    Found email(s) on contact page (mailto): {filtered}")
                        return filtered
                break
            except Exception:
                continue
    
    return []


def process_album(album_url: str, session: requests.Session) -> dict:
    """Process a single album: scrape page, check external site if needed. Returns result dict."""
    result = {
        "emails": [],
        "logs": [],
        "error": None,
        "rate_limited": False,
    }
    
    try:
        emails, ext_url, artist_name, bio_text, logs, rate_limited = scrape_album_page(album_url, session)
        result["logs"] = logs
        result["rate_limited"] = rate_limited
        
        if rate_limited:
            time.sleep(5)  # Back off on rate limit
            return result
        
        if emails:
            for email in emails:
                result["emails"].append({
                    "email": email,
                    "name": artist_name,
                    "notes": bio_text[:500] if bio_text else "",
                    "source_url": album_url,
                })
        elif ext_url:
            ext_emails = scrape_external_site(ext_url, session, result["logs"])
            for email in ext_emails:
                result["emails"].append({
                    "email": email,
                    "name": artist_name,
                    "notes": bio_text[:500] if bio_text else "",
                    "source_url": album_url,
                })
        
        time.sleep(0.3)  # Brief pause between requests
        
    except Exception as e:
        result["error"] = f"Error scraping {album_url}: {e}"
    
    return result


def main():
    parser = argparse.ArgumentParser(description="Bandcamp Email Scraper")
    parser.add_argument("--tag", default="", help="Bandcamp tag to scrape")
    parser.add_argument("--clicks", type=int, default=3, help="Number of 'View more' clicks")
    parser.add_argument("--workers", type=int, default=1, help="Concurrent album scrapers")
    parser.add_argument("--output", choices=["json", "csv"], default="json", help="Output format")
    parser.add_argument("--headless", type=str, default="true", help="Run headless browser")
    args = parser.parse_args()
    
    headless = args.headless.lower() in ("true", "1", "yes")
    workers = max(1, min(args.workers, 10))  # Clamp between 1-10
    
    log("=" * 50)
    log("Bandcamp Email Scraper")
    log("=" * 50)
    log(f"Tag: {args.tag or '(generic)'}")
    log(f"Clicks: {args.clicks}")
    log(f"Workers: {workers}")
    log(f"Headless: {headless}")
    log("=" * 50)
    
    results = []
    errors = []
    
    # Discover albums
    try:
        album_urls = discover_album_urls(args.tag, args.clicks, headless)
    except Exception as e:
        errors.append(f"Discovery failed: {e}")
        album_urls = []
    
    # Create shared session for connection pooling
    session = requests.Session()
    session.headers.update(HEADERS)
    
    # Scrape albums concurrently
    log(f"\nScraping {len(album_urls)} albums with {workers} workers...\n")
    
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = {executor.submit(process_album, url, session): url for url in album_urls}
        
        for future in as_completed(futures):
            album_result = future.result()
            
            # Output grouped logs for this album
            if album_result["logs"]:
                log("\n".join(album_result["logs"]))
            
            # Collect results
            results.extend(album_result["emails"])
            
            if album_result["error"]:
                errors.append(album_result["error"])
    
    # Deduplicate by email
    seen = set()
    unique_results = []
    for r in results:
        if r["email"] not in seen:
            seen.add(r["email"])
            unique_results.append(r)
    
    log("=" * 50)
    log(f"Scraping complete: {len(unique_results)} unique emails found")
    log("=" * 50)
    
    # Output
    output = {
        "success": True,
        "tag": args.tag,
        "total_albums_scraped": len(album_urls),
        "results": unique_results,
        "errors": errors,
    }
    
    if args.output == "json":
        print(json.dumps(output, indent=2))
    else:
        # CSV output
        import csv
        import io
        
        buffer = io.StringIO()
        writer = csv.DictWriter(buffer, fieldnames=["email", "name", "notes", "source_url"])
        writer.writeheader()
        for r in unique_results:
            writer.writerow(r)
        print(buffer.getvalue())


if __name__ == "__main__":
    main()
