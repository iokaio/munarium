# HX-200 bench analyser: error code table

Document HM-ERR, applies to revisions A and B.

| Code | Meaning | First action |
|---|---|---|
| E10 | Lamp not detected | Check lamp seating |
| E22 | Temperature out of range | Check the sample temperature sensor |
| E41 | Pump stall | Prime the sample path; replace the pump if it persists |
| E42 | Pump overcurrent | Replace the pump |
| E47 | Pump flow below minimum | Prime the sample path; check tubing |
| E53 | Host communication lost | Check the data cable |

Codes E41, E42 and E47 are pump faults. Persistent pump faults lead to the
pump replacement procedure.
