# Why I made Helia

I work in high-performance computing, and I tend to think about problems the way an HPC person does. You do not sit and watch a job. You automate the work, put it in a queue, let the machine get on with it in batches, and come back to review the results. If they are not good enough, you adjust the requirements and submit again. Doing that loop a few hundred times is how you find out what really matters.

My astrophotography went the same way. Before Helia, my solar processing was a workflow of my own, mostly Python, which I built and used on my own captures, around that habit: captures went in as a batch, the processing ran without me, and I looked at what came out and decided what to change. Much of that workflow has now been converted into Helia, as fast, tested software. That is why it is not only a window with buttons. A good part of what is in it is there because my own pipeline needed it first.

## How the habits became features

**Automate it and run it unattended.** `helia stack` needs only an input and an output, takes every setting as a flag, and exits with a non-zero status when it fails, so it runs from a shell script, a cron job or a job scheduler on a machine with no screen. The [batch recipe](recipes/batch-processing.md) is the loop I would have written by hand anyway: every capture in a folder, a failure in one never hiding the others, a log for each. Helia reads the filter from the file name and picks the matching preset, so one command suits a mixed night.

**Queue it.** If you want a queue that other programs can add to, `helia serve` is a small HTTP and WebSocket service with a job queue. A job is an input, an output and a block of settings. You get an id back, follow the progress, and collect the result. It runs one job at a time by default, because the GPU is a single device, and it answers "queue full" instead of falling over. The [API tutorial](tutorials/api-server.md) walks through it from a script.

**Review the results as data.** `helia stack --timings` says where the time went. `helia measure` puts a number on how much granulation a picture holds, so two settings can be compared on the same capture without arguing with your own eyes. The sunspot, time-lapse and mosaic tools write CSV (and JSON for the sunspots) beside the pictures, which go straight into a spreadsheet or a notebook, and the [seeing report](user-guide/quality-report.md) tells good captures from poor ones before you spend an evening on them. (Helia does not write an HTML report; the results are the files and the numbers.)

**Adjust and resubmit.** Every setting is a flag or a field in the job, so trying a different number of frames to keep, a different alignment point size or a different sharpening is a change of one word and another run. Presets give a sensible starting point for each filter. The FITS file records which version of Helia made it and how the capture was read, so a result can be traced back to how it was made.

**Use the machine you have.** The heavy stages run on a GPU when there is one, through Vulkan, DX12 or Metal, and `HELIA_GPU_DEVICE` chooses the card when several are installed or another job already has the first. With no GPU it uses all the CPU cores. The [performance guide](user-guide/performance.md) has measured numbers and says how they were measured.

## What this does not mean

The desktop app is real, and it is what most people will use. It walks you through the steps with a picture at each one. The command line and the API are not afterthoughts bolted on to it: all three are front ends on the same engine. If you only ever click through the app, you lose nothing. If you ever have a hundred captures and a free weekend, the other doors are open.

## Dedication

Helia is dedicated to my daughter, Leilú, and takes its name from her middle name.

> Helia is dedicated to my daughter, the light of my life. Her middle name is
> Helia, because she has an energetic spark. She has not stayed still since the
> day she was born, which suits software whose whole job is catching the sharp
> moments in something that never stops moving.

I would like Helia to be useful to people I will never meet, and I would like to be a smaller bottleneck in my own work. Both seem like good reasons to build it.

Laurence Horrocks-Barlow
