* Common-emitter amplifier, divider bias, bypassed emitter (shared by the bjt_ce_amp_* decks)
.include models.inc
VCC vcc 0 12
VIN in 0 DC 0 AC 1 SIN(0 10m 1k 0 0 0)
CIN in b 10u
R1 vcc b 47k
R2 b 0 10k
RC vcc c 4.7k
RE e 0 1k
CE e 0 100u
COUT c out 10u
RL out 0 10k
Q1 c b e QN
