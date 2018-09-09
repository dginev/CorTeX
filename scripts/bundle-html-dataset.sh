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

## Obtain the task lists
psql -h $PGADDRESS -U cortex -t -o "$DTPATH/$CORPUSNAME-no_problem-tasks.txt" -c "SELECT entry FROM tasks WHERE corpus_id=$CORPUSID and service_id=$SERVICEID and status=-1"
psql -h $PGADDRESS -U cortex -t -o "$DTPATH/$CORPUSNAME-warning-tasks.txt" -c "SELECT entry FROM tasks WHERE corpus_id=$CORPUSID and service_id=$SERVICEID and status=-2"
psql -h $PGADDRESS -U cortex -t -o "$DTPATH/$CORPUSNAME-error-tasks.txt" -c "SELECT entry FROM tasks WHERE corpus_id=$CORPUSID and service_id=$SERVICEID and status=-3"

# For each severity, prepare a dataset archive of HTML files
severitylist="no_problem warning error"

for severity in $severitylist; do
    mkdir $DTPATH/$severity
    egrep -o '.+\/' $DTPATH/$CORPUSNAME-$severity-tasks.txt | while read -r line ; do
        YEARDIR=$(expr match $line "^$CORPUSBASE/\([0-9]*\)")
        SUBDIR=$(expr match $line "^$CORPUSBASE/[0-9]*/\([a-z0-9._-]*\)")
        FULLDIR=$(expr match $line "^\($CORPUSBASE/[0-9]*/[a-z0-9._-]*\)")

        FILENAME=$(unzip $FULLDIR/tex_to_html.zip *.html -d $DTPATH/$severity | egrep -o '\S*\.html')
        if [ -f $FILENAME ]
        then
            if [ ! -d "$DTPATH/$severity/$YEARDIR" ]; then
                mkdir $DTPATH/$severity/$YEARDIR
            fi
            mv $FILENAME $DTPATH/$severity/$YEARDIR/$SUBDIR.html
        fi
    done

    # Create the final dataset archive
    zip -9 -r $DTPATH/$CORPUSNAME-$severity.zip $DTPATH/$severity/ || exit 1;

    rm -rf $DTPATH/$severity
done

exit 0;
