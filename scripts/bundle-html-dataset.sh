#!/bin/bash

# Prerequisite:
#  PGPASSWORD=<the cortex user password>
#  PGADDRESS=<the postgresql ip|localhost>
#  DTPATH=<the path for the created dataset>
#  CORPUSNAME=<the name of the corpus>
#  CORPUSID=<PG database id of this corpus>
#  CORPUSBASE=<base file system path of the corpus>
#  SERVICEID=<PG database id of this service>
#
# Example:
# PGPASSWORD=cortex PGADDRESS=10.188.48.220 CORPUSNAME=arxmliv CORPUSID=8 SERVICEID=3 CORPUSBASE=/data/arxmliv DTPATH=/data/datasets/dataset-arXMLiv-08-2018 ./scripts/bundle-html-dataset.sh

mkdir -p $DTPATH

echo "1. Obtain the task lists..."

psql -h $PGADDRESS -U cortex -t -o "$DTPATH/$CORPUSNAME-no_problem-tasks.txt" -c "SELECT entry FROM tasks WHERE corpus_id=$CORPUSID and service_id=$SERVICEID and status=-1 order by entry"
psql -h $PGADDRESS -U cortex -t -o "$DTPATH/$CORPUSNAME-warning-tasks.txt" -c "SELECT entry FROM tasks WHERE corpus_id=$CORPUSID and service_id=$SERVICEID and status=-2 order by entry"
psql -h $PGADDRESS -U cortex -t -o "$DTPATH/$CORPUSNAME-error-tasks.txt" -c "SELECT entry FROM tasks WHERE corpus_id=$CORPUSID and service_id=$SERVICEID and status=-3 order by entry"

echo "2. Unpack into yymm (year-month) directories..."

all_years="91 92 93 94 95 96 97 98 99 00 01 02 03 04 05 06 07 08 09 10 11 12 13 14 15 16 17 18 19 20"
all_months="01 02 03 04 05 06 07 08 09 10 11 12"
severitylist="no_problem warning error"

for yy in $all_years; do
    for mm in $all_months; do
        yymm="$yy$mm"
        # make it resumable from partially completed .zip state
        if [ -f "$DTPATH/$CORPUSNAME-$yymm.zip" ] ; then
            continue
        fi
        echo "-- copy papers for $yymm"
        for severity in $severitylist; do
            egrep -o ".+\/$yymm\/.+\/" $DTPATH/$CORPUSNAME-$severity-tasks.txt | while read -r line ; do
                SUBDIR=$(expr match $line "^$CORPUSBASE/[0-9]*/\([a-z0-9._-]*\)")
                FULLDIR=$(expr match $line "^\($CORPUSBASE/[0-9]*/[a-z0-9._-]*\)")
                HTMLFILE="$DTPATH/$yymm/$SUBDIR.html"
                if [ ! -d "$DTPATH/$yymm" ] ; then
                    mkdir $DTPATH/$yymm
                fi
                if [ -f $HTMLFILE ] ; then # skip unzipping existing files
                  continue
                fi
                FILENAME=$(unzip -n $FULLDIR/tex_to_html.zip *.html -d $DTPATH/$yymm | egrep -o '\S*\.html')
                if [[ -f $FILENAME ]] && [[ "$FILENAME" != "$HTMLFILE" ]] ;
                then
                    mv -f $FILENAME $HTMLFILE
                fi
            done
        done
        if [ -d "$DTPATH/$yymm" ]; then
            echo "-- create archive for $yymm"
            cd $DTPATH
            zip -9 -v -r $CORPUSNAME-$yymm.zip $yymm
            echo "-- free space for $yymm"
            rm -rf $DTPATH/$yymm
        fi
    done
done

echo "Done!"
exit 0;
