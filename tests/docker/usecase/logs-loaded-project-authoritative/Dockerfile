FROM sysg-usecase-base

COPY tests/docker/usecase/logs-loaded-project-authoritative/one.yaml /usecase/one.yaml
COPY tests/docker/usecase/logs-loaded-project-authoritative/two.yaml /usecase/two.yaml
COPY tests/docker/usecase/logs-loaded-project-authoritative/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
