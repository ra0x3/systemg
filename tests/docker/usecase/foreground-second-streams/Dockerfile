FROM sysg-usecase-base

COPY tests/docker/usecase/foreground-second-streams/alpha.yaml /usecase/alpha.yaml
COPY tests/docker/usecase/foreground-second-streams/beta.yaml /usecase/beta.yaml
COPY tests/docker/usecase/foreground-second-streams/fgcap.py /usecase/fgcap.py
COPY tests/docker/usecase/foreground-second-streams/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
