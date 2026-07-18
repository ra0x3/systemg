FROM sysg-usecase-base

COPY tests/docker/usecase/foreground-ctrlc-isolated/aa.yaml /usecase/aa.yaml
COPY tests/docker/usecase/foreground-ctrlc-isolated/bb.yaml /usecase/bb.yaml
COPY tests/docker/usecase/foreground-ctrlc-isolated/fgctl.py /usecase/fgctl.py
COPY tests/docker/usecase/foreground-ctrlc-isolated/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
