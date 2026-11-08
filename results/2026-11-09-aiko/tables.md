| Offered | Client | Achieved req/s | Planned − sent in window | p50 µs | p99 µs | p99.9 µs | Dispatch lag p99 µs | Schedule lag p99 µs | Busy | Softirq share | Flagged | Load average |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 400k | after | 400,000 (397,130–400,001) | 0 (0–1,228,553) | 4.2 (4.1–9,269,411.8) | 42.1 (6.6–14,914,945.0) | 796.7 (49.9–15,023,852.9) | 1.9 (0.3–5.8) | 7.5 (0.6–8,376.3) | 0.688 (0.647–0.832) | 0.233 (0.090–0.265) | 3/5 | 7.4–96.0 |
| 400k | before | 400,000 (399,998–400,002) | 0 (0–884,963) | 4.1 (4.1–4,991,221.8) | 10.6 (6.5–9,000,976.4) | 383.7 (21.9–9,126,805.5) | 0.8 (0.5–4.0) | – | 0.703 (0.679–0.765) | – | 0/5 | 6.1–83.0 |
| 400k | fixed | 151,834 (138,042–400,000) | 992,665 (0–1,047,842) | 5,100,273.7 (4.3–5,335,154.7) | 9,361,686.5 (7.4–9,789,505.5) | 9,412,018.2 (72.6–9,865,003.0) | 101.6 (1.0–121.9) | – | – | – | – | 10.7–103.0 |
| 500k | after | 500,000 (499,918–500,001) | 0 (0–1,498,492) | 3.7 (3.6–8,304,721.9) | 11.8 (9.9–11,760,828.4) | 2,349.1 (77.7–11,851,107.3) | 1.9 (0.3–31.0) | 2.5 (1.6–6,111.2) | 0.822 (0.750–0.849) | 0.268 (0.095–0.273) | 5/5 | 12.1–96.1 |
| 500k | before | 500,000 (367,511–500,001) | 6 (0–1,618,457) | 3.7 (3.7–10,645,143.6) | 1,908.7 (73.5–15,929,966.6) | 2,994.2 (719.9–16,047,407.1) | 12.9 (0.5–33.4) | – | 0.879 (0.731–1.000) | – | 1/5 | 8.2–104.1 |
| 500k | fixed | 500,000 (267,608–500,000) | 0 (0–929,506) | 3.9 (3.9–447,742.0) | 15.5 (6.7–8,141,144.1) | 272.4 (19.9–8,178,892.8) | 3.7 (1.0–91.4) | – | – | – | – | 8.5–97.3 |
| 600k | after | 600,000 (476,355–600,109) | 1,418,904 (-4,211–1,857,830) | 3,321,888.8 (3.4–10,301,210.6) | 5,888,802.8 (126.5–15,627,976.7) | 5,934,940.2 (1,161.2–15,804,137.5) | 0.3 (0.3–18.8) | 3,076.1 (29.4–7,405.6) | 0.674 (0.606–0.996) | 0.185 (0.110–0.315) | 5/5 | 7.1–86.5 |
| 600k | before | 408,544 (322,221–600,001) | 1,990,349 (-1–2,103,196) | 10,829,692.9 (3.7–11,987,320.8) | 16,164,847.6 (52.2–16,735,273.0) | 16,273,899.5 (292.4–16,844,324.9) | 0.4 (0.4–18.8) | – | 0.693 (0.609–0.998) | – | 2/5 | 13.9–76.0 |
| 600k | fixed | 153,040 (111,413–600,000) | 1,787,862 (0–1,954,356) | 4,324,327.4 (3.7–9,739,173.9) | 5,435,818.0 (28.7–15,376,318.5) | 5,473,566.7 (333.6–15,476,981.8) | 56.6 (2.9–108.2) | – | – | – | – | 13.5–107.0 |
| 650k | after | 420,434 (338,260–650,002) | 2,068,224 (0–2,295,874) | 11,072,962.6 (3.7–12,138,315.8) | 16,341,008.4 (243.1–16,760,438.8) | 16,408,117.2 (1,919.0–16,903,045.1) | 0.3 (0.3–23.3) | 5,103.6 (105.0–10,469.4) | 0.729 (0.611–0.999) | 0.098 (0.055–0.307) | 5/5 | 12.8–92.3 |
| 650k | before | 650,001 (649,762–650,003) | 182,366 (-1–588,919) | 309.5 (6.6–935,329.8) | 510,394.4 (403.2–3,250,585.6) | 543,162.4 (1,232.9–3,384,803.3) | 19.7 (0.4–33.2) | – | 0.991 (0.883–0.999) | – | 4/5 | 6.0–88.8 |
| 650k | fixed | 650,000 (650,000–650,001) | -1 (-1–-1) | 6.0 (6.0–6.0) | 28.6 (23.4–33.7) | 646.5 (291.6–1,001.5) | 7.0 (6.2–7.8) | – | – | – | – | 11.4–102.4 |
| 700k | after | 699,997 (513,854–700,115) | 479,668 (1–2,291,032) | 783,810.6 (9.3–10,636,754.9) | 807,350.1 (2,748.4–15,971,909.6) | 807,350.1 (3,203.1–16,055,795.7) | 8.1 (0.2–31.5) | 245.0 (157.7–8,921.1) | 0.940 (0.637–0.999) | 0.295 (0.095–0.320) | 5/5 | 5.6–86.5 |
| 700k | before | 700,000 (297,714–700,003) | 105,060 (46,715–2,394,006) | 121,962.5 (37,355.5–11,559,501.8) | 190,316.5 (68,026.4–17,230,200.8) | 190,603.5 (68,146.7–17,297,309.7) | 0.2 (0.2–0.4) | – | 0.999 (0.680–1.000) | – | 3/5 | 9.5–97.3 |
| 700k | fixed | 700,000 (264,783–700,000) | 0 (0–1,740,849) | 12.2 (8.6–3,258,974.2) | 5,287.9 (3,074.0–4,643,094.5) | 14,753.8 (6,438.9–4,685,037.6) | 50.3 (44.9–77.7) | – | – | – | – | 11.9–87.1 |

Reasons (after):
- 400k round 1: the 99th percentile schedule lag, 8.376 ms, exceeds 50.00 µs; 1228553 of the 1600000 requests planned in the window (76.78%) were not sent in it
- 400k round 2: not saturated
- 400k round 3: worker 0 was busy for 83.2% of the window
- 400k round 4: the 99th percentile schedule lag, 369.66 µs, exceeds 50.00 µs; 3133 of the 1600000 requests planned in the window (0.20%) were not sent in it
- 400k round 5: not saturated
- 500k round 1: worker 0 was busy for 84.4% of the window
- 500k round 2: the 99th percentile schedule lag, 6.111 ms, exceeds 50.00 µs; 1498492 of the 2000000 requests planned in the window (74.92%) were not sent in it
- 500k round 3: worker 0 was busy for 84.9% of the window; the 99th percentile schedule lag, 204.41 µs, exceeds 50.00 µs
- 500k round 4: worker 0 was busy for 82.0% of the window
- 500k round 5: worker 0 was busy for 82.2% of the window
- 600k round 1: the 99th percentile schedule lag, 7.406 ms, exceeds 50.00 µs; 1857830 of the 2400000 requests planned in the window (77.41%) were not sent in it
- 600k round 2: the 99th percentile schedule lag, 5.026 ms, exceeds 50.00 µs; 1844797 of the 2400000 requests planned in the window (76.87%) were not sent in it
- 600k round 3: worker 0 was busy for 97.8% of the window
- 600k round 4: worker 0 was busy for 99.6% of the window; the 99th percentile schedule lag, 154.88 µs, exceeds 50.00 µs
- 600k round 5: the 99th percentile schedule lag, 3.076 ms, exceeds 50.00 µs; 1418904 of the 2400000 requests planned in the window (59.12%) were not sent in it
- 650k round 1: the 99th percentile schedule lag, 9.642 ms, exceeds 50.00 µs; 2295874 of the 2600000 requests planned in the window (88.30%) were not sent in it
- 650k round 2: the 99th percentile schedule lag, 10.469 ms, exceeds 50.00 µs; 2294048 of the 2600000 requests planned in the window (88.23%) were not sent in it
- 650k round 3: the 99th percentile schedule lag, 5.104 ms, exceeds 50.00 µs; 2068224 of the 2600000 requests planned in the window (79.55%) were not sent in it
- 650k round 4: worker 0 was busy for 99.9% of the window; the 99th percentile schedule lag, 104.96 µs, exceeds 50.00 µs
- 650k round 5: worker 0 was busy for 99.9% of the window; the 99th percentile schedule lag, 191.10 µs, exceeds 50.00 µs
- 700k round 1: the 99th percentile schedule lag, 8.921 ms, exceeds 50.00 µs; 2108334 of the 2800000 requests planned in the window (75.30%) were not sent in it
- 700k round 2: the 99th percentile schedule lag, 5.849 ms, exceeds 50.00 µs; 2291032 of the 2800000 requests planned in the window (81.82%) were not sent in it
- 700k round 3: worker 0 was busy for 94.0% of the window; the 99th percentile schedule lag, 244.99 µs, exceeds 50.00 µs; 479668 of the 2800000 requests planned in the window (17.13%) were not sent in it
- 700k round 4: worker 0 was busy for 99.9% of the window; the 99th percentile schedule lag, 157.69 µs, exceeds 50.00 µs
- 700k round 5: worker 0 was busy for 99.0% of the window; the 99th percentile schedule lag, 173.57 µs, exceeds 50.00 µs; 58700 of the 2800000 requests planned in the window (2.10%) were not sent in it
