FROM sysg-usecase-base

COPY tests/docker/usecase/status-added-project-tree/alpha.yaml /usecase/alpha.yaml
COPY tests/docker/usecase/status-added-project-tree/beta.yaml /usecase/beta.yaml
COPY tests/docker/usecase/status-added-project-tree/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
