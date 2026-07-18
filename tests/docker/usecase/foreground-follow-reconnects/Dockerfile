FROM sysg-usecase-base

COPY tests/docker/usecase/foreground-follow-reconnects/anchor.yaml /usecase/anchor.yaml
COPY tests/docker/usecase/foreground-follow-reconnects/beta.yaml /usecase/beta.yaml
COPY tests/docker/usecase/foreground-follow-reconnects/fgcap.py /usecase/fgcap.py
COPY tests/docker/usecase/foreground-follow-reconnects/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
