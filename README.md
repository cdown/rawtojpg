# rawtojpg

rawtojpg provides a much faster way to extract embedded JPEGs from RAW files
than exiftool's `-JpgFromRaw`. In a directory with ~1200 files, rawtojpg
extracts JPEGs about 17 times faster than exiftool:

    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v rawtojpg /mnt/sdcard/DCIM/101MSDCF ~/rtj/rawtojpg >/dev/null
            User time (seconds): 0.43
            System time (seconds): 3.39
            Percent of CPU this job got: 91%
            Elapsed (wall clock) time (h:mm:ss or m:ss): 0:04.19

    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v exiftool -b -JpgFromRaw -ext raw -r /mnt/sdcard/DCIM/101MSDCF -w ~/rtj/exiftool/%f.jpg >/dev/null
            User time (seconds): 25.89
            System time (seconds): 8.95
            Percent of CPU this job got: 46%
            Elapsed (wall clock) time (h:mm:ss or m:ss): 1:14.77
