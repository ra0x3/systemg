FROM sysg-usecase-base

COPY tests/docker/usecase/foreground-daemon-supervisor-stop/daemon.yaml /usecase/daemon.yaml
COPY tests/docker/usecase/foreground-daemon-supervisor-stop/fg.yaml /usecase/fg.yaml
COPY tests/docker/usecase/foreground-daemon-supervisor-stop/fg_run.py /usecase/fg_run.py
COPY tests/docker/usecase/foreground-daemon-supervisor-stop/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
