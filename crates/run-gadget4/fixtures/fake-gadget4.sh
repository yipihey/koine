#!/bin/sh
# A stand-in for GADGET-4 for the adapter's tests. It does what the adapter
# relies on, in the shapes the real code writes (recorded from a real run of
# Gadget4-capi, see ../README.md):
#   - the startup banner, compile settings and parameter echo on stdout;
#   - <OutputDir>/parameters-usedvalues and ./param.txt-usedvalues;
#   - one `Sync-Point` line per step in <OutputDir>/info.txt (and on stdout) and a
#     `MEMORY:` line in memory.txt;
#   - snapshot_000.hdf5 (a real, tiny HDF5 file with Config/Parameters/Header,
#     $FAKE_SNAPSHOT) after the first step;
#   - restartfiles/restart.0 at the end, on a `stop` file and on a `restart` file;
#   - `stop` in the output dir: write restart, delete it, say so, exit 0.
# Arguments after the parameter file, key=value (as GADGET-4's own would be
# `0` or `1 7`): steps=N (3), sleep=SECONDS (0.2 per step), snapshot=FILE,
# fail=param (refuse the parameter file, exit 0 as GADGET-4 does) or
# fail=nan (Terminate on NaN, exit 1 as MPI_Abort does), config=short (a
# compile-settings list that disagrees with the snapshot's Config group).
param="$1"
shift
steps=3
nap=0.2
FAKE_SNAPSHOT=""
FAKE_FAIL=""
FAKE_CONFIG=full
for kv in "$@"; do
  case "$kv" in
    steps=*) steps="${kv#steps=}" ;;
    sleep=*) nap="${kv#sleep=}" ;;
    snapshot=*) FAKE_SNAPSHOT="${kv#snapshot=}" ;;
    fail=*) FAKE_FAIL="${kv#fail=}" ;;
    config=*) FAKE_CONFIG="${kv#config=}" ;;
  esac
done
out=$(awk '$1=="OutputDir"{print $2}' "$param")
base=$(awk '$1=="SnapshotFileBase"{print $2}' "$param")
a0=$(awk '$1=="TimeBegin"{print $2}' "$param")

echo "This is Gadget, version 4.0."
echo "Git commit 6e4f1128ac696260d3f2aff68cb6fa7b335e4c1c, Tue May 20 19:00:00 2025 +0200"
echo
echo "Code was compiled with the following settings:"
if [ "$FAKE_CONFIG" = "short" ]; then
  # a compile-settings list that disagrees with the snapshot's Config group
  echo "    PERIODIC"
  echo "    PMGRID=512"
else
  for c in ASMTH=2.0 CREATE_GRID DOUBLEPRECISION=1 FOF GADGET2_HEADER IDS_32BIT NGENIC=256 \
           NGENIC_2LPT NSOFTCLASSES=1 NTYPES=2 PERIODIC PMGRID=256 RANDOMIZE_DOMAINCENTER \
           SELFGRAVITY SUBFIND TREEPM_NOTIMESPLIT; do
    echo "    $c"
  done
fi
echo
echo "Obtaining parameters from file '$param':"
echo
awk '!/^[%#]/ && NF>=2 {printf "        %-50s%s\n", $1, $2}' "$param"
echo

if [ "${FAKE_FAIL}" = "param" ]; then
  echo "Error in file $param:   Tag 'Bogus' not allowed"
  echo "We stop because of an error in the parameterfile"
  exit 0
fi

mkdir -p "$out"
awk '!/^[%#]/ && NF>=2 {printf "%-50s%s\n", $1, $2}' "$param" > "$out/parameters-usedvalues"
cp "$out/parameters-usedvalues" "$param-usedvalues"

write_restart() {
  mkdir -p "$out/restartfiles"
  echo "fake restart at step $1" > "$out/restartfiles/restart.0"
}

i=0
while [ "$i" -lt "$steps" ]; do
  if [ -e "$out/stop" ]; then
    rm -f "$out/stop"
    echo "stop-file detected. stopping."
    write_restart "$i"
    echo "endrun called, calling MPI_Finalize()"
    exit 0
  fi
  if [ -e "$out/restart" ]; then
    rm -f "$out/restart"
    echo "restart-file detected. writing restart files."
    write_restart "$i"
  fi
  if [ "${FAKE_FAIL}" = "nan" ] && [ "$i" -eq 1 ]; then
    echo "TERMINATE: ******!!!!!******  Code termination on task=0, function drift(), file src/x.cc, line 1:"
    echo "found NaN in the position of particle 12"
    exit 1
  fi
  t=$(awk -v a="$a0" -v i="$i" 'BEGIN{printf "%.6g", a + i*0.001}')
  line=$(awk -v t="$t" -v i="$i" 'BEGIN{z=1/t-1; printf "Sync-Point %d, Time: %s, Redshift: %.6g, Systemstep: 0.001, Dloga: 0.00297844, Nsync-grv:       4096, Nsync-hyd:          0", i, t, z}')
  echo "$line" >> "$out/info.txt"
  echo >> "$out/info.txt"
  echo "$line"
  echo "MEMORY:  Largest Allocation = $((10 + i)).5 Mbyte  |  Largest Allocation Without Generic = 1 Mbyte" >> "$out/memory.txt"
  if [ "$i" -eq 1 ] && [ -n "${FAKE_SNAPSHOT}" ]; then
    cp "$FAKE_SNAPSHOT" "$out/${base}_000.hdf5"
  fi
  sleep "$nap"
  i=$((i + 1))
done
echo "Final time reached. Simulation ends."
write_restart "$i"
echo "endrun called, calling MPI_Finalize()"
echo "bye!"
exit 0
