#!/usr/bin/env fish
# SPDX-License-Identifier: GPL-3.0-or-later
#
# cooling-sample.fish — sample CPU/GPU temperature and fan RPM while a
# load runs, for calibrating the fan-curve presets (see
# docs/dev/FAN_CURVE_TESTING.md §3).
#
# WHAT IT DOES
#   Every few seconds it reads, straight from sysfs (so it does not
#   depend on hpd):
#     • CPU temp   — k10temp  / temp1_input (Tctl)
#     • GPU temp   — amdgpu   / temp1_input (edge)
#     • CPU/GPU RPM — asus     / fan1_input, fan2_input
#   It prints a row per sample and, at the end, the PEAK of each.
#   hwmon nodes are found by NAME (indices are not stable across boots).
#
# HOW TO USE
#   1. In terminal A, start a sustained all-core load:
#        for i in (seq 8); yes >/dev/null & ; end
#      (or, if installed:  stress-ng --cpu 0 --timeout 120s)
#   2. In terminal B, for EACH cooling level, set it and run this script:
#        hpdctl cool set silent     ; fish docs/dev/cooling-sample.fish
#        hpdctl cool set balanced   ; fish docs/dev/cooling-sample.fish
#        hpdctl cool set aggressive ; fish docs/dev/cooling-sample.fish
#        hpdctl cool reset          ; fish docs/dev/cooling-sample.fish   # firmware baseline
#   3. Stop the load:  kill (jobs -p)
#   4. Record each run's PEAK line in the §3 table and send it over.
#
#   Optional real-power reading (needs root + the tool):
#        sudo ryzenadj -i | grep -Ei 'STAPM|PPT'
#
# Usage: fish cooling-sample.fish [seconds] [interval]   (default 90 5)

set -l total 90
set -l interval 5
test (count $argv) -ge 1; and set total $argv[1]
test (count $argv) -ge 2; and set interval $argv[2]

function _hwmon --argument-names name
    for d in /sys/class/hwmon/hwmon*
        if test (cat $d/name 2>/dev/null) = $name
            echo $d
            return 0
        end
    end
end

set -l K10 (_hwmon k10temp)
set -l AGPU (_hwmon amdgpu)
set -l ASUS (_hwmon asus)

test -z "$K10"; and echo "warning: k10temp (CPU temp) node not found"
test -z "$ASUS"; and echo "warning: asus (fan RPM) node not found"

set -l maxc 0
set -l maxg 0
set -l maxr1 0
set -l maxr2 0

printf "%6s  %5s  %5s  %8s  %8s\n" time CPUc GPUc CPUrpm GPUrpm

set -l t 0
while test $t -le $total
    set -l c 0
    test -n "$K10"; and set c (math -s0 (cat $K10/temp1_input) / 1000)
    set -l g 0
    test -n "$AGPU"; and set g (math -s0 (cat $AGPU/temp1_input) / 1000)
    set -l r1 0
    test -n "$ASUS"; and set r1 (cat $ASUS/fan1_input 2>/dev/null; or echo 0)
    set -l r2 0
    test -n "$ASUS"; and set r2 (cat $ASUS/fan2_input 2>/dev/null; or echo 0)

    test $c -gt $maxc; and set maxc $c
    test $g -gt $maxg; and set maxg $g
    test $r1 -gt $maxr1; and set maxr1 $r1
    test $r2 -gt $maxr2; and set maxr2 $r2

    printf "%5ss  %5s  %5s  %8s  %8s\n" $t $c $g $r1 $r2
    sleep $interval
    set t (math $t + $interval)
end

echo "──────── PEAK ────────"
printf "CPU %s°C   GPU %s°C   CPU fan %s rpm   GPU fan %s rpm\n" $maxc $maxg $maxr1 $maxr2
