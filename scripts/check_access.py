#!/usr/bin/env python3
"""Check robots.txt for every source ajs uses or has considered.

Prints one line per endpoint: allowed / DISALLOWED / no robots.txt /
unknown, plus any Crawl-delay. Re-run it before adding a source and from
time to time for the existing ones, then update the "Access policy"
table in SOURCES.md.

robots.txt is only one signal: a documented public API or a published
feed counts as permission too, and an anti-bot challenge counts as a
refusal even when robots.txt allows the path.

Usage: python3 scripts/check_access.py
"""
import urllib.error
import urllib.parse
import urllib.request
import urllib.robotparser

# Matches the product token sent by src/http.rs.
UA = "ajs"

CHECKS = [
    # Crawlers in use
    ("cryptocurrencyjobs feed", "https://cryptocurrencyjobs.co/index.xml"),
    ("cryptocurrencyjobs detail", "https://cryptocurrencyjobs.co/engineering/some-job/"),
    ("cryptojobs feed", "https://www.cryptojobs.com/jobs/feed"),
    ("hn algolia", "https://hn.algolia.com/api/v1/items/1"),
    ("thehub list", "https://thehub.io/jobs"),
    ("thehub detail", "https://thehub.io/jobs/0123456789abcdef01234567"),
    ("web3career", "https://web3.career/remote-jobs"),
    ("workingnomads api", "https://www.workingnomads.com/api/exposed_jobs/"),
    ("workingnomads redirect", "https://www.workingnomads.com/job/go/123/"),
    # ATS adapters in use
    ("greenhouse api", "https://boards-api.greenhouse.io/v1/boards/x/jobs"),
    ("ashby api", "https://api.ashbyhq.com/posting-api/job-board/x"),
    ("lever api", "https://api.lever.co/v0/postings/x"),
    ("smartrecruiters api", "https://api.smartrecruiters.com/v1/companies/x/postings"),
    ("bamboohr list", "https://sololearn.bamboohr.com/careers/list"),
    ("recruitee api", "https://exeon.recruitee.com/api/offers/"),
    ("workday cxs", "https://nvidia.wd5.myworkdayjobs.com/wday/cxs/nvidia/NVIDIAExternalCareerSite/jobs"),
    # Candidate ATS endpoints
    ("workable widget", "https://apply.workable.com/api/v1/widget/accounts/modash"),
    ("breezy json", "https://bitwave.breezy.hr/json"),
    ("teamtailor rss", "https://axmed.teamtailor.com/jobs.rss"),
    ("jazzhr list", "https://greenzie.applytojob.com/apply/jobs"),
    ("personio xml", "https://eventfirst.jobs.personio.de/xml"),
    ("rippling api", "https://api.rippling.com/platform/api/ats/v1/board/rippling/jobs"),
    ("gem api", "https://api.gem.com/job_board/v0/gem/job_posts/"),
    ("grnh.se redirect", "https://grnh.se/abc"),
    # Candidate aggregators
    ("himalayas api", "https://himalayas.app/jobs/api"),
    ("himalayas job page", "https://himalayas.app/companies/x/jobs/y"),
    ("remoteok api", "https://remoteok.com/api"),
    ("remoteok job page", "https://remoteok.com/remote-jobs/x-1"),
    ("wwr rss", "https://weworkremotely.com/remote-jobs.rss"),
    ("wwr job page", "https://weworkremotely.com/remote-jobs/x"),
    ("remotive api", "https://remotive.com/api/remote-jobs"),
    ("jobicy api", "https://jobicy.com/api/v2/remote-jobs"),
    ("arbeitnow api", "https://www.arbeitnow.com/api/job-board-api"),
    ("commoncrawl index", "https://index.commoncrawl.org/CC-MAIN-2026-43-index"),
    ("workatastartup", "https://www.workatastartup.com/companies"),
    ("wellfound", "https://wellfound.com/jobs"),
    ("dover", "https://app.dover.com/jobs/x"),
    ("getro (a16z)", "https://jobs.a16z.com/jobs"),
]


def check(url):
    parts = urllib.parse.urlsplit(url)
    robots = f"{parts.scheme}://{parts.netloc}/robots.txt"
    req = urllib.request.Request(robots, headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            body = resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        if e.code in (404, 410):
            return "no robots.txt"
        return f"unknown (robots.txt HTTP {e.code})"
    except Exception as e:
        return f"unknown ({type(e).__name__})"
    rp = urllib.robotparser.RobotFileParser()
    rp.parse(body.splitlines())
    verdict = "allowed" if rp.can_fetch(UA, url) else "DISALLOWED"
    delay = rp.crawl_delay(UA)
    return f"{verdict} crawl-delay={delay}" if delay else verdict


def main():
    for name, url in CHECKS:
        print(f"{name:28} {check(url):36} {url}")


if __name__ == "__main__":
    main()
