# Apify Actors for Following Fetch

Apify MCP is the fallback collection method when CDP (agent-browser) is unavailable.

## Recommended Actors

### 1. apidojo/twitter-following-scraper (preferred)

**Input**:
```json
{
  "usernames": ["n_asuy"],
  "maxItems": 500
}
```

**Output field mapping**:

| Actor field | Canonical field |
|-------------|-----------------|
| `screen_name` | `username` |
| `name` | `display_name` |
| `description` | `bio` |
| `followers_count` | `followers_count` |
| `friends_count` | `following_count` |
| `verified` | `verified` |
| `profile_image_url` | `avatar_url` |

---

### 2. microworlds/twitter-followers-following-scraper

**Input**:
```json
{
  "handles": ["n_asuy"],
  "mode": "following",
  "maxItems": 500
}
```

**Output field mapping**:

| Actor field | Canonical field |
|-------------|-----------------|
| `userName` | `username` |
| `displayName` | `display_name` |
| `bio` | `bio` |
| `followersCount` | `followers_count` |
| `followingCount` | `following_count` |
| `isVerified` | `verified` |
| `profileUrl` | `profile_url` |
| `profileImageUrl` | `avatar_url` |

---

### 3. web_scraping_pro/twitter-following-list-scraper

**Input**:
```json
{
  "username": "n_asuy",
  "limit": 500
}
```

**Output field mapping**:

| Actor field | Canonical field |
|-------------|-----------------|
| `username` | `username` |
| `name` | `display_name` |
| `description` | `bio` |
| `followers` | `followers_count` |
| `following` | `following_count` |
| `verified` | `verified` |
| `url` | `profile_url` |
| `avatar` | `avatar_url` |

---

## Search Queries

If none of the above actors are available:

```
search-actors: "twitter following"
search-actors: "x following scraper"
search-actors: "twitter friends list"
```

## Limitation

Apify actors **cannot detect `follows_you` status**. All accounts collected via Apify will have `follows_you: false`, reducing the accuracy of keep_score in the analysis step. CDP collection is preferred when possible.

## Error Codes

| Code | Meaning | Action |
|------|---------|--------|
| RATE_LIMITED | Rate limit reached | Wait and retry |
| USER_NOT_FOUND | User does not exist | Verify username |
| PRIVATE_ACCOUNT | Account is private | Public accounts only |
| ACTOR_FAILED | Actor execution failed | Try alternate actor |
