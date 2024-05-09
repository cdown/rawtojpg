# arwtojpg

arwtojpg provides a much faster way to extract embedded JPEGs from Sony ARW
files than exiftool's `-JpgFromRaw`. In a directory with ~1200 files, arwtojpg
extracts JPEGs about 17 times faster than exiftool:

    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v arwtojpg /mnt/sdcard/DCIM/101MSDCF ~/atj/arwtojpg >/dev/null
            User time (seconds): 0.43
            System time (seconds): 3.39
            Percent of CPU this job got: 91%
            Elapsed (wall clock) time (h:mm:ss or m:ss): 0:04.19

    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v exiftool -b -JpgFromRaw -ext arw -r /mnt/sdcard/DCIM/101MSDCF -w ~/atj/exiftool/%f.jpg >/dev/null
            User time (seconds): 25.89
            System time (seconds): 8.95
            Percent of CPU this job got: 46%
            Elapsed (wall clock) time (h:mm:ss or m:ss): 1:14.77

This is much faster because, compared to exiftool:

1. arwtojpg uses fadvise/madvise to avoid reading too much of the large ARW
   file unnecessarily due to readahead;
2. arwtojpg can process multiple files concurrently;
3. arwtojpg can process reading and writing concurrently.
