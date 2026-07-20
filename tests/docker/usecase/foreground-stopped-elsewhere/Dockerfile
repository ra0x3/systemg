FROM sysg-usecase-base

COPY tests/docker/usecase/foreground-stopped-elsewhere/alpha.yaml /usecase/alpha.yaml
COPY tests/docker/usecase/foreground-stopped-elsewhere/beta.yaml /usecase/beta.yaml
COPY tests/docker/usecase/foreground-stopped-elsewhere/fgwait.py /usecase/fgwait.py
COPY tests/docker/usecase/foreground-stopped-elsewhere/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
