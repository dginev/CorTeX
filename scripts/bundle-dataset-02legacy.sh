#!/bin/bash

# Prerequisite:
# export PGPASSWORD=<the cortex user password>
BASE=$(pwd)

## Obtain the task lists (legacy arxiv setup: corpusid is 4, serviceid is 3 no_problem is -1, warning is -2, error is -3)
psql -U cortex -t -o 'no_problem_tasks.txt' -c 'SELECT entry FROM tasks WHERE corpusid=4 and serviceid=3 and status=-1'
psql -U cortex -t -o 'warning_tasks.txt' -c 'SELECT entry FROM tasks WHERE corpusid=4 and serviceid=3 and status=-2'
psql -U cortex -t -o 'error_tasks.txt' -c 'SELECT entry FROM tasks WHERE corpusid=4 and serviceid=3 and status=-3'

## No problems files
mkdir $BASE/no_problem
egrep -o '.+\/' $BASE/no_problem_tasks.txt | while read -r line ; do
    YEARDIR=$(expr match $line '^/arXMLiv/modern/\([0-9]*\)')
    SUBDIR=$(expr match $line '^/arXMLiv/modern/[0-9]*/\([a-z0-9._-]*\)')

    FILENAME=$BASE/$(unzip $line/tex_to_html.zip *.html -d no_problem | egrep -o '\S*\.html')
    if [ -f $FILENAME ]
    then
        if [ ! -d "$BASE/no_problem/$YEARDIR" ]; then
            mkdir $BASE/no_problem/$YEARDIR
        fi
        mv $FILENAME $BASE/no_problem/$YEARDIR/$SUBDIR.html
    fi
done

## Warning files
mkdir $BASE/warning
egrep -o '.+\/' $BASE/warning_tasks.txt | while read -r line ; do
    YEARDIR=$(expr match $line '^/arXMLiv/modern/\([0-9]*\)')
    SUBDIR=$(expr match $line '^/arXMLiv/modern/[0-9]*/\([a-z0-9._-]*\)')

    FILENAME=$BASE/$(unzip $line/tex_to_html.zip *.html -d warning | egrep -o '\S*\.html')
    if [ -f $FILENAME ]
    then
        if [ ! -d "$BASE/warning/$YEARDIR" ]; then
            mkdir $BASE/warning/$YEARDIR
        fi
        mv $FILENAME $BASE/warning/$YEARDIR/$SUBDIR.html
    fi
done

## Error files
mkdir $BASE/error
egrep -o '.+\/' $BASE/error_tasks.txt | while read -r line ; do
    YEARDIR=$(expr match $line '^/arXMLiv/modern/\([0-9]*\)')
    SUBDIR=$(expr match $line '^/arXMLiv/modern/[0-9]*/\([a-z0-9._-]*\)')

    FILENAME=$BASE/$(unzip $line/tex_to_html.zip *.html -d error | egrep -o '\S*\.html')
    if [ -f $FILENAME ]
    then
        if [ ! -d "$BASE/error/$YEARDIR" ]; then
            mkdir $BASE/error/$YEARDIR
        fi
        mv $FILENAME $BASE/error/$YEARDIR/$SUBDIR.html
    fi
done

# Create the final dataset archive
zip dataset.zip no_problem/ warning/ error/ || exit 1;

rm -rf no_problem warning error
exit 0;
