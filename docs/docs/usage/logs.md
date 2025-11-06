---
sidebar_position: 4
title: logs
---

| ⚠️ Note that on Unix-like systems, the logs command is currently not supported.   |
|----------------------------------------------|

View the last 50 lines of logs for all services.

```sh
$ sysg logs
```

View the logs for a specific service.

```sh
$ sysg logs --lines 100
```

```sh
$ sysg logs

+---------------------------------+
|         arb-rs (138246)         |
+---------------------------------+

==> /home/ubuntu/.local/share/systemg/logs/arb-rs_stdout.log <==
2025-11-06T15:21:42.341828Z DEBUG request{method=GET uri=/api/v1/stadiums?cache=true&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connecting to 146.75.78.132:443
2025-11-06T15:21:42.343746Z DEBUG request{method=GET uri=/api/v1/scores?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connecting to 146.75.78.132:443
2025-11-06T15:21:42.344003Z  INFO request{method=GET uri=/api/v1/team-profile?cache=true&league=nfl version=HTTP/1.1}: arb_rs::uses::sportradar: Making real API request for NFL team profile: https://api.sportsdata.io/v3/nfl/scores/json/AllTeams
2025-11-06T15:21:42.344324Z DEBUG request{method=GET uri=/api/v1/team-profile?cache=true&league=nfl version=HTTP/1.1}: reqwest::connect: starting new connection: https://api.sportsdata.io/
2025-11-06T15:21:42.344562Z  INFO request{method=GET uri=/api/v1/schedule?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: arb_rs::uses::sportradar: No cached schedule data found for league nfl, will fetch from origin
2025-11-06T15:21:42.344580Z  INFO request{method=GET uri=/api/v1/schedule?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: arb_rs::uses::sportradar: Fetching schedule data from API: https://api.sportsdata.io/v3/nfl/scores/json/ScoresByDate/2025-11-06
2025-11-06T15:21:42.344586Z  INFO request{method=GET uri=/api/v1/schedule?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: arb_rs::uses::sportradar: Using GamesByDate endpoint for date: 2025-11-06
2025-11-06T15:21:42.344596Z  INFO request{method=GET uri=/api/v1/schedule?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: arb_rs::uses::sportradar: Fetching games by date data from API: https://api.sportsdata.io/v3/nfl/scores/json/ScoresByDate/2025-11-06 for date: 2025-11-06
2025-11-06T15:21:42.345701Z DEBUG request{method=GET uri=/api/v1/team-profile?cache=true&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connecting to 146.75.78.132:443
2025-11-06T15:21:42.349834Z DEBUG request{method=GET uri=/api/v1/stadiums?cache=true&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connected to 146.75.78.132:443
2025-11-06T15:21:42.351637Z DEBUG request{method=GET uri=/api/v1/scores?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connected to 146.75.78.132:443
2025-11-06T15:21:42.353601Z DEBUG request{method=GET uri=/api/v1/team-profile?cache=true&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connected to 146.75.78.132:443
2025-11-06T15:21:42.428944Z  INFO request{method=GET uri=/api/v1/schedule?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: arb_rs::uses::sportradar: Making real API request for NFL games by date: https://api.sportsdata.io/v3/nfl/scores/json/ScoresByDate/2025-11-06
2025-11-06T15:21:42.429297Z DEBUG request{method=GET uri=/api/v1/schedule?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: reqwest::connect: starting new connection: https://api.sportsdata.io/
```

